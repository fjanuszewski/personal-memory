//! El guardián de la memoria (punto 4.8 del diseño): toma las mejoras que
//! el usuario aceptó, hace trabajar al agente en una copia, prueba lo que hizo, lo
//! instala y vuelve atrás si algo falla. El agente no puede tocar este programa.

pub mod agente;
pub mod base;
pub mod bateria;
pub mod config;
pub mod git;
pub mod instalar;
pub mod instancia;
pub mod mejora;
pub mod orden;
pub mod revision;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::agente::ClaudeCode;
use crate::config::Config;
use crate::mejora::Guardian;

/// Cada cuánto se mira si hay mejoras aceptadas.
const CADA: Duration = Duration::from_secs(5);
/// Cada cuánto se prueba de nuevo la base si no responde, y cada cuánto se avisa.
const REINTENTO_BASE: Duration = Duration::from_secs(15);
const AVISO_SIN_BASE: Duration = Duration::from_mins(10);

/// Abre la base y, si todavía no está (la crea el servicio la primera vez
/// que arranca), sigue probando sin irse. Se avisa cada tanto, no en cada
/// intento.
async fn conectar(cfg: &Config, apagado: &CancellationToken) -> anyhow::Result<SqlitePool> {
    let mut ultimo_aviso: Option<tokio::time::Instant> = None;
    loop {
        let intento = async {
            let pool = base::abrir(&cfg.bd).await?;
            sqlx::query("select count(*) from mejoras").fetch_one(&pool).await?;
            anyhow::Ok(pool)
        };
        match intento.await {
            Ok(pool) => return Ok(pool),
            Err(falla) => {
                if ultimo_aviso.is_none_or(|cuando| cuando.elapsed() >= AVISO_SIN_BASE) {
                    ultimo_aviso = Some(tokio::time::Instant::now());
                    tracing::warn!(
                        falla = format!("{falla:#}"),
                        base = %cfg.bd.display(),
                        "todavía no puedo usar la base de la memoria (la crea el servicio al arrancar): sigo probando"
                    );
                }
            }
        }
        tokio::select! {
            () = apagado.cancelled() => anyhow::bail!("me apagaron antes de poder abrir la base"),
            () = tokio::time::sleep(REINTENTO_BASE) => {},
        }
    }
}

/// Atiende hasta que llegue la señal de apagado.
pub async fn atender(cfg: Config, apagado: CancellationToken) -> anyhow::Result<()> {
    let pool = conectar(&cfg, &apagado).await?;
    std::fs::create_dir_all(cfg.trabajo()).context("no pude crear la carpeta de trabajo")?;
    let a_la_vez = cfg.a_la_vez;
    let agente = Arc::new(ClaudeCode::nuevo(cfg.clone()));
    let guardian = Arc::new(Guardian::nuevo(cfg, pool, agente));
    let retomadas = guardian.recuperar().await?;
    tracing::info!(retomadas, a_la_vez, "guardián atento a las mejoras aceptadas");

    let cupo = Arc::new(Semaphore::new(a_la_vez));
    loop {
        while let Ok(lugar) = Arc::clone(&cupo).try_acquire_owned() {
            match guardian.tomar().await {
                Ok(Some(mejora)) => {
                    let guardian = Arc::clone(&guardian);
                    tokio::spawn(async move {
                        guardian.atender_una(&mejora).await;
                        drop(lugar);
                    });
                }
                Ok(None) => break,
                Err(falla) => {
                    tracing::warn!(falla = format!("{falla:#}"), "no pude mirar las mejoras aceptadas");
                    break;
                }
            }
        }
        tokio::select! {
            () = apagado.cancelled() => break,
            () = tokio::time::sleep(CADA) => {},
        }
    }
    Ok(())
}

/// Corre la batería a pedido, sin instalar ni cambiar nada: prende
/// `programa` (o el instalado) aparte, sobre una copia de la base real (o
/// sobre una vacía si todavía no hay ninguna), y devuelve cómo le fue.
pub async fn medir(cfg: &Config, programa: Option<&std::path::Path>) -> anyhow::Result<bateria::Corrida> {
    let programa = programa.map_or_else(|| cfg.programa(), std::path::Path::to_owned);
    std::fs::create_dir_all(cfg.trabajo()).context("no pude crear la carpeta de trabajo")?;
    let dir = cfg.trabajo().join(format!("medicion-{}", std::process::id()));
    let foto = cfg.trabajo().join(format!("medicion-{}-foto.db", std::process::id()));
    let hay_base = cfg.bd.is_file();
    if hay_base {
        base::volcar(cfg, &cfg.bd, &foto).await?;
    }
    let prendida =
        instancia::prender(cfg, &programa, hay_base.then_some(foto.as_path()), cfg.puerto_pruebas + 199, &dir).await;
    let corrida = match prendida {
        Ok(instancia) => {
            let corrida = match instancia.esperar_huellas().await {
                Ok(()) => bateria::correr(&instancia).await,
                Err(falla) => Err(falla),
            };
            instancia.apagar().await;
            corrida
        }
        Err(falla) => Err(falla),
    };
    let _ = std::fs::remove_dir_all(&dir);
    let _ = base::borrar(cfg, &foto);
    corrida
}
