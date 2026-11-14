//! Instalar el programa nuevo y volver atrás si el servicio no queda sano
//! (punto 4.8 del diseño, paso 6). Se instala de a una mejora por vez.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;

use crate::base;
use crate::config::Config;
use crate::orden;

/// Cuánto se vigila el servicio después de reiniciarlo.
const VIGILANCIA: Duration = Duration::from_secs(30);
const CADA: Duration = Duration::from_secs(2);
/// Lo que se espera antes de mirar, para no confundir al programa viejo, que
/// todavía se está cerrando, con el nuevo.
const RESPIRO: Duration = Duration::from_secs(2);
const LIMITE: Duration = Duration::from_secs(60);

fn sistema(
    cfg: &Config,
    programa: &str,
    args: &[&str],
) -> impl Future<Output = anyhow::Result<String>> + Send + 'static + use<> {
    let corrida = orden::exigir(Path::new(programa), args, &cfg.dir, &cfg.entorno(), LIMITE);
    async move { Ok(corrida.await?.salida.trim().to_owned()) }
}

async fn dominio(cfg: &Config) -> anyhow::Result<String> {
    Ok(format!("gui/{}", sistema(cfg, "/usr/bin/id", &["-u"]).await?))
}

/// Reinicia el servicio instalado (o lo prende si estaba apagado).
async fn reiniciar(cfg: &Config) -> anyhow::Result<()> {
    if let Some(orden) = &cfg.reinicio {
        sistema(cfg, "/bin/sh", &["-c", orden]).await?;
        return Ok(());
    }
    let dominio = dominio(cfg).await?;
    let servicio = format!("{dominio}/{}", cfg.etiqueta);
    if sistema(cfg, "/bin/launchctl", &["print", &servicio]).await.is_err() {
        sistema(cfg, "/bin/launchctl", &["bootstrap", &dominio, &cfg.plist().display().to_string()]).await?;
    }
    sistema(cfg, "/bin/launchctl", &["kickstart", "-k", &servicio]).await?;
    Ok(())
}

/// Apaga el servicio y lo deja apagado (para poder restaurar la base).
async fn apagar(cfg: &Config) -> anyhow::Result<()> {
    if let Some(orden) = &cfg.apagado {
        sistema(cfg, "/bin/sh", &["-c", orden]).await?;
        return Ok(());
    }
    let servicio = format!("{}/{}", dominio(cfg).await?, cfg.etiqueta);
    sistema(cfg, "/bin/launchctl", &["bootout", &servicio]).await?;
    Ok(())
}

async fn sano(http: &reqwest::Client, cfg: &Config) -> bool {
    match http.get(format!("{}/salud", cfg.servicio)).send().await {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.is_ok_and(|v| v["ok"] == true),
        _ => false,
    }
}

/// Vigila el servicio recién reiniciado: tiene que quedar sano dentro del
/// plazo y, una vez sano, no volver a caerse.
async fn vigilar(cfg: &Config) -> bool {
    let Ok(http) = reqwest::Client::builder().timeout(Duration::from_secs(3)).build() else { return false };
    tokio::time::sleep(RESPIRO).await;
    let fin = tokio::time::Instant::now() + VIGILANCIA;
    let mut anduvo = false;
    loop {
        let ahora = sano(&http, cfg).await;
        if anduvo && !ahora {
            return false;
        }
        anduvo |= ahora;
        if tokio::time::Instant::now() >= fin {
            return anduvo;
        }
        tokio::time::sleep(CADA).await;
    }
}

fn copiar(desde: &Path, hasta: &Path) -> anyhow::Result<()> {
    // Se copia al lado y se renombra: el programa nunca queda a medio escribir.
    let temporal = hasta.with_extension("nuevo");
    std::fs::copy(desde, &temporal).with_context(|| format!("no pude copiar {}", desde.display()))?;
    std::fs::rename(&temporal, hasta).with_context(|| format!("no pude reemplazar {}", hasta.display()))
}

/// Deja todo como estaba antes de instalar: el programa anterior y, si el
/// nuevo llegó a cambiar la estructura de la base, la base del respaldo.
async fn volver_atras(cfg: &Config, id: i64, respaldo: &Path, migracion_antes: i64) -> anyhow::Result<bool> {
    let programa = cfg.programa();
    apagar(cfg).await.context("no pude apagar el servicio para volver atrás")?;
    copiar(&programa.with_extension("anterior"), &programa)?;
    // Si no se puede ni preguntar, se restaura: es lo seguro.
    let cambio_la_base = base::version_migraciones(&cfg.bd).await.map_or(true, |ahora| ahora != migracion_antes);
    if cambio_la_base {
        let apodo = format!("mejora_{id}_{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
        let fallida = base::volver_a(cfg, respaldo, &apodo).await?;
        tracing::warn!(mejora = id, fallida = %fallida.display(), "la base volvió al respaldo; la que falló quedó guardada aparte");
    }
    reiniciar(cfg).await.context("no pude prender de nuevo el servicio anterior")?;
    Ok(cambio_la_base)
}

/// Instala `candidato` como el programa del servicio. Si no queda sano,
/// vuelve atrás. `Err` trae el motivo, dicho para el usuario.
pub async fn instalar(cfg: &Config, id: i64, candidato: &Path) -> Result<(), String> {
    let texto = |falla: anyhow::Error| format!("{falla:#}");
    std::fs::create_dir_all(cfg.respaldos()).map_err(|e| e.to_string())?;
    let respaldo = cfg
        .respaldos()
        .join(format!("memoria-{}-antes-de-mejora-{id}.db", chrono::Local::now().format("%Y-%m-%d-%H%M%S")));
    base::volcar(cfg, &cfg.bd, &respaldo)
        .await
        .map_err(|e| format!("No pude respaldar la base antes de instalar, así que no instalé nada: {}", texto(e)))?;
    let migracion_antes = base::version_migraciones(&cfg.bd).await.map_err(texto)?;

    let programa = cfg.programa();
    std::fs::copy(&programa, programa.with_extension("anterior"))
        .map_err(|e| format!("No pude guardar el programa anterior, así que no instalé nada: {e}"))?;
    copiar(candidato, &programa).map_err(texto)?;

    let prendido = reiniciar(cfg).await;
    if prendido.is_ok() && vigilar(cfg).await {
        tracing::info!(mejora = id, respaldo = %respaldo.display(), "mejora instalada");
        return Ok(());
    }
    tracing::warn!(mejora = id, "el servicio no quedó sano con el cambio: vuelvo atrás");
    match volver_atras(cfg, id, &respaldo, migracion_antes).await {
        Ok(con_base) => {
            let sano_otra_vez = vigilar(cfg).await;
            if !sano_otra_vez {
                tracing::error!(mejora = id, "volví al programa anterior pero el servicio no responde");
            }
            Err(format!(
                "Con el cambio instalado el servicio no quedó sano, así que volví al programa anterior{}.{}",
                if con_base { " y a la base de antes" } else { "" },
                if sano_otra_vez { "" } else { " Ojo: el servicio todavía no responde; hay que mirarlo." }
            ))
        }
        Err(falla) => {
            tracing::error!(mejora = id, falla = format!("{falla:#}"), "no pude volver atrás");
            Err(format!(
                "Con el cambio instalado el servicio no quedó sano y no pude volver atrás solo: {}. El respaldo está en {}.",
                texto(falla),
                respaldo.display()
            ))
        }
    }
}
