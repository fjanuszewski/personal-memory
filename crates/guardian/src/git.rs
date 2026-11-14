//! La copia del código de cada mejora: un `git worktree` con su rama.
//!
//! Todo pasa en la copia del código que dejó el instalador en la carpeta de
//! instalación (`codigo`), no en el repositorio donde se desarrolla: una
//! mejora puede traer ejemplos de lo que cada uno buscó, y eso no tiene que
//! llegar solo a un repositorio que se comparte.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};

use crate::config::Config;
use crate::orden;
use crate::revision::{Cambio, leer_cambios};

const LIMITE: Duration = Duration::from_secs(120);
/// Lo que escribe el instalador cuando el programa instalado salió de código
/// que no estaba guardado en git.
pub const SIN_GUARDAR: &str = "sin-guardar";

pub fn git(
    cfg: &Config,
    dir: &Path,
    args: &[&str],
) -> impl Future<Output = anyhow::Result<String>> + Send + 'static + use<> {
    let corrida = orden::exigir(Path::new("git"), args, dir, &cfg.entorno(), LIMITE);
    async move { Ok(corrida.await?.salida.trim().to_owned()) }
}

/// El commit del que salió el programa instalado: de ahí parte cada mejora.
pub async fn version_instalada(cfg: &Config) -> anyhow::Result<String> {
    let marca = std::fs::read_to_string(cfg.marca_instalada()).unwrap_or_default();
    let marca = marca.trim();
    if marca.is_empty() || marca == SIN_GUARDAR {
        bail!(
            "No sé de qué versión del código salió el programa instalado (tenía cambios sin guardar en git). Hay que guardar los cambios y correr el instalador de nuevo."
        );
    }
    git(cfg, &cfg.repo, &["cat-file", "-e", &format!("{marca}^{{commit}}")])
        .await
        .with_context(|| format!("la versión instalada ({marca}) no está en {}", cfg.repo.display()))?;
    Ok(marca.to_owned())
}

pub fn anotar_instalada(cfg: &Config, commit: &str) -> anyhow::Result<()> {
    std::fs::write(cfg.marca_instalada(), format!("{commit}\n")).context("no pude anotar la versión instalada")
}

/// Donde se compila una mejora: una carpeta propia al lado de su copia del
/// código. No se comparte: con dos mejoras a la vez, una pisaría el programa
/// recién compilado de la otra.
#[must_use]
pub fn compilados_de(copia: &Path) -> PathBuf {
    let nombre = copia.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    copia.with_file_name(format!("{nombre}-compilado"))
}

/// Lo ya compilado de la última versión instalada: cada mejora arranca de una
/// copia de esto para no compilar todas las librerías de nuevo.
#[must_use]
pub fn compilados_base(cfg: &Config) -> PathBuf {
    cfg.trabajo().join("compilado-base")
}

/// Arma la copia de trabajo de una mejora, partiendo de `base`. Si había una
/// de un intento anterior, la reemplaza.
pub async fn copia_nueva(cfg: &Config, id: i64, intento: i32, base: &str) -> anyhow::Result<(PathBuf, String)> {
    let dir = cfg.trabajo().join(format!("mejora-{id}"));
    quitar_copia(cfg, &dir).await;
    std::fs::create_dir_all(cfg.trabajo())?;
    let rama = format!("mejora-{id}-{intento}");
    git(cfg, &cfg.repo, &["worktree", "add", "--force", "-B", &rama, &dir.display().to_string(), base]).await?;
    // En este disco la copia es instantánea y no ocupa lugar hasta que algo cambia.
    let (semilla, compilados) = (compilados_base(cfg), compilados_de(&dir));
    let _ = std::fs::remove_dir_all(&compilados);
    if semilla.is_dir() {
        let args = ["-Rc", &semilla.display().to_string(), &compilados.display().to_string()];
        let copia = orden::correr(Path::new("/bin/cp"), &args, &cfg.dir, &cfg.entorno(), LIMITE);
        if !copia.await.is_ok_and(|c| c.ok) {
            let _ = std::fs::remove_dir_all(&compilados);
        }
    }
    Ok((dir, rama))
}

/// Saca la copia de trabajo. La rama queda en el repositorio.
pub async fn quitar_copia(cfg: &Config, dir: &Path) {
    if dir.exists() {
        let _ = git(cfg, &cfg.repo, &["worktree", "remove", "--force", &dir.display().to_string()]).await;
        let _ = std::fs::remove_dir_all(dir);
    }
    let _ = git(cfg, &cfg.repo, &["worktree", "prune"]).await;
}

/// Qué archivos cambió el agente en la copia.
pub async fn cambios(cfg: &Config, dir: &Path) -> anyhow::Result<Vec<Cambio>> {
    let salida =
        orden::exigir(Path::new("git"), &["status", "--porcelain=v1", "-uall"], dir, &cfg.entorno(), LIMITE).await?;
    Ok(leer_cambios(&salida.salida))
}

/// Guarda en la rama de la mejora todo lo que cambió. Devuelve el commit.
pub async fn guardar(cfg: &Config, dir: &Path, mensaje: &str) -> anyhow::Result<String> {
    git(cfg, dir, &["add", "-A"]).await?;
    git(cfg, dir, &["commit", "--quiet", "--no-verify", "-m", mensaje]).await?;
    git(cfg, dir, &["rev-parse", "HEAD"]).await
}

/// Adelanta la rama de la copia del código (`instalada`) a la mejora que se
/// acaba de instalar, si está limpia y justo en la versión anterior. Si no,
/// no toca nada: la mejora queda en su rama.
pub async fn adelantar_principal(cfg: &Config, antes: &str, rama: &str) -> bool {
    let limpio = git(cfg, &cfg.repo, &["status", "--porcelain"]).await.is_ok_and(|s| s.is_empty());
    let actual = git(cfg, &cfg.repo, &["rev-parse", "HEAD"]).await.unwrap_or_default();
    let en_rama = git(cfg, &cfg.repo, &["symbolic-ref", "--short", "HEAD"]).await.is_ok();
    limpio && en_rama && actual == antes && git(cfg, &cfg.repo, &["merge", "--ff-only", "--quiet", rama]).await.is_ok()
}
