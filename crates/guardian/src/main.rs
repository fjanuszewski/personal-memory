use std::fs;

use anyhow::Context;
use guardian::config::Config;
use tokio_util::sync::CancellationToken;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Registros en `~/Library/Logs/memoria/guardian.<fecha>.log` (14 días) y los
/// avisos también por la salida de errores, que launchd guarda aparte.
fn iniciar_registros(cfg: &Config) -> anyhow::Result<WorkerGuard> {
    fs::create_dir_all(&cfg.registros).with_context(|| format!("no pude crear {}", cfg.registros.display()))?;
    let archivo = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("guardian")
        .filename_suffix("log")
        .max_log_files(14)
        .build(&cfg.registros)
        .context("no pude abrir el archivo de registros")?;
    let (escritor, guardia) = tracing_appender::non_blocking(archivo);
    let filtro = EnvFilter::try_from_env("MEMORIA_REGISTRO").unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(escritor).with_ansi(false).with_filter(filtro))
        .with(fmt::layer().with_writer(std::io::stderr).with_filter(LevelFilter::WARN))
        .init();
    Ok(guardia)
}

async fn esperar_apagado() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
}

/// `memoria-guardian bateria [programa]`: mide la búsqueda con las preguntas
/// de la batería, sin tocar lo instalado.
async fn bateria(cfg: &Config, programa: Option<String>) -> anyhow::Result<()> {
    let programa = programa.map(std::path::PathBuf::from);
    let corrida = guardian::medir(cfg, programa.as_deref()).await?;
    println!("Batería: {} de {} preguntas bien.", corrida.aprobadas(), corrida.resultados.len());
    for fallada in corrida.falladas() {
        println!("  no pasó: {}", fallada.split_once(':').map_or(fallada, |(_, pregunta)| pregunta));
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::desde_entorno()?;
    let mut argumentos = std::env::args().skip(1);
    match argumentos.next().as_deref() {
        None => {}
        Some("bateria") => return bateria(&cfg, argumentos.next()).await,
        Some(otro) => anyhow::bail!("no conozco «{otro}»: sin nada atiende las mejoras; «bateria» mide la búsqueda"),
    }
    let _guardia = iniciar_registros(&cfg)?;
    let apagado = CancellationToken::new();
    let fin = apagado.clone();
    tokio::spawn(async move {
        esperar_apagado().await;
        fin.cancel();
    });
    if let Err(falla) = guardian::atender(cfg, apagado).await {
        tracing::error!(falla = format!("{falla:#}"), "el guardián se detuvo por un error");
        return Err(falla);
    }
    tracing::info!("guardián apagado");
    Ok(())
}
