//! Lanzar otros programas (`git`, `cargo` y demás) con un entorno
//! limpio, un límite de tiempo y la salida guardada.

use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail};
use tokio::process::Command;

/// Cuánto de la salida se guarda para mostrar si algo falla.
const COLA: usize = 4000;

#[derive(Debug)]
pub struct Salida {
    pub ok: bool,
    pub salida: String,
    pub errores: String,
}

impl Salida {
    /// El final de lo que dijo el programa, para explicar una falla.
    #[must_use]
    pub fn cola(&self) -> String {
        let todo = format!("{}\n{}", self.salida.trim_end(), self.errores.trim_end());
        let todo = todo.trim();
        let total = todo.chars().count();
        if total <= COLA {
            todo.to_owned()
        } else {
            format!("…{}", todo.chars().skip(total - COLA).collect::<String>())
        }
    }
}

/// Corre `programa` en `dir` con solo las variables de `entorno`. Si pasa de
/// `limite` lo corta, a él y a lo que haya lanzado.
///
/// La orden se arma al llamar; lo que se espera después no guarda nada
/// prestado, así se puede usar desde cualquier tarea.
pub fn correr(
    programa: &Path,
    args: &[&str],
    dir: &Path,
    entorno: &[(&str, String)],
    limite: Duration,
) -> impl Future<Output = anyhow::Result<Salida>> + Send + 'static + use<> {
    let mut orden = Command::new(programa);
    orden
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(entorno.iter().map(|(clave, valor)| (*clave, valor.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let nombre = programa.display().to_string();
    async move {
        let hijo = orden.spawn().with_context(|| format!("no pude lanzar {nombre}"))?;
        let grupo = hijo.id();
        let Ok(resultado) = tokio::time::timeout(limite, hijo.wait_with_output()).await else {
            // El programa principal ya murió al soltarlo; esto alcanza a sus hijos.
            if let Some(grupo) = grupo {
                let _ = Command::new("/bin/kill").args(["-KILL", "--", &format!("-{grupo}")]).status().await;
            }
            bail!("{nombre} tardó más de {} min y lo corté", limite.as_secs() / 60)
        };
        let r = resultado.with_context(|| format!("falló la espera de {nombre}"))?;
        Ok(Salida {
            ok: r.status.success(),
            salida: String::from_utf8_lossy(&r.stdout).into_owned(),
            errores: String::from_utf8_lossy(&r.stderr).into_owned(),
        })
    }
}

/// Como [`correr`], pero una falla del programa es un error con su salida.
pub fn exigir(
    programa: &Path,
    args: &[&str],
    dir: &Path,
    entorno: &[(&str, String)],
    limite: Duration,
) -> impl Future<Output = anyhow::Result<Salida>> + Send + 'static + use<> {
    let corrida = correr(programa, args, dir, entorno, limite);
    let que = format!("{} {}", programa.display(), args.join(" "));
    async move {
        let salida = corrida.await?;
        if !salida.ok {
            bail!("{que} falló: {}", salida.cola());
        }
        Ok(salida)
    }
}
