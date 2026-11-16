use std::fs;

use anyhow::Context;
use memoria::config::Config;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Registros en `~/Library/Logs/memoria/memoria.<fecha>.log` (14 días) y los
/// avisos también por la salida de errores, que launchd guarda aparte.
fn iniciar_registros(cfg: &Config) -> anyhow::Result<WorkerGuard> {
    fs::create_dir_all(&cfg.registros).with_context(|| format!("no pude crear {}", cfg.registros.display()))?;
    let archivo = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("memoria")
        .filename_suffix("log")
        .max_log_files(14)
        .build(&cfg.registros)
        .context("no pude abrir el archivo de registros")?;
    let (escritor, guardia) = tracing_appender::non_blocking(archivo);
    let filtro =
        EnvFilter::try_from_env("MEMORIA_REGISTRO").unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,rmcp=warn"));
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(escritor).with_ansi(false).with_filter(filtro))
        .with(fmt::layer().with_writer(std::io::stderr).with_filter(LevelFilter::WARN))
        .init();
    Ok(guardia)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::desde_entorno()?;
    match std::env::args().nth(1).as_deref() {
        None => {}
        // Lo que corre el instalador: deja lista la base y el modelo de huellas, y sale.
        Some("preparar") => {
            println!("{}", memoria::preparar(&cfg).await?);
            return Ok(());
        }
        Some("version" | "--version") => {
            println!("memoria {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        // Para hacer a mano lo que el servicio hace solo cada unas horas: lee
        // ahora las fuentes encendidas y sale. Con «--probar» no guarda nada.
        Some("juntar") => {
            let probar = match std::env::args().nth(2).as_deref() {
                None => false,
                Some("--probar") => true,
                Some(otro) => anyhow::bail!("«juntar» no conoce «{otro}»: va solo o con «--probar»"),
            };
            let (bien, resumen) = memoria::juntar(&cfg, probar).await?;
            println!("{resumen}");
            if !bien {
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(otro) => {
            anyhow::bail!(
                "no conozco «{otro}»: sin nada arranca el servicio; «preparar» deja lista la base y el modelo; \
                 «juntar» lee las fuentes encendidas"
            )
        }
    }
    let _guardia = iniciar_registros(&cfg)?;
    if let Err(e) = memoria::arrancar(cfg).await {
        tracing::error!(error = format!("{e:#}"), "memoria se detuvo por un error");
        return Err(e);
    }
    Ok(())
}
