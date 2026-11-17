//! Servicio «memoria»: memoria a largo plazo por grupo (empresa › proyecto)
//! para las conversaciones con Claude. Ver `docs/diseno.md`.

pub mod base;
pub mod buscar;
pub mod clave;
pub mod config;
pub mod contexto;
pub mod error;
pub mod estado;
pub mod estilo;
pub mod grupos;
pub mod huellas;
pub mod indice;
pub mod interno;
pub mod lector;
pub mod mcp;
pub mod mejoras;
pub mod modelo;
pub mod notas;
pub mod respaldo;
pub mod senales;
pub mod texto;
pub mod web;

use std::sync::Arc;

use anyhow::Context;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, Proveedor};
use crate::estado::Estado;
use crate::huellas::Huellas;
use crate::interno::Interno;

/// Las migraciones de `migraciones/`, embebidas en el programa.
pub static MIGRACIONES: Migrator = sqlx::migrate!("../../migraciones");

/// Aplica las migraciones pendientes, con un respaldo antes si la base ya
/// tenía datos.
pub async fn migrar(pool: &SqlitePool, cfg: &Config) -> anyhow::Result<()> {
    let tablas: i64 =
        sqlx::query_scalar("select count(*) from sqlite_master where type = 'table' and name = '_sqlx_migrations'")
            .fetch_one(pool)
            .await?;
    let aplicadas: Vec<i64> = if tablas > 0 {
        sqlx::query_scalar("select version from _sqlx_migrations where success").fetch_all(pool).await?
    } else {
        Vec::new()
    };
    let pendientes: Vec<i64> = MIGRACIONES
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && !aplicadas.contains(&m.version))
        .map(|m| m.version)
        .collect();
    let Some(primera) = pendientes.first() else {
        return Ok(());
    };
    if !aplicadas.is_empty() {
        let archivo = respaldo::antes_de_migrar(pool, cfg, *primera).await?;
        tracing::info!(archivo = %archivo.display(), "respaldo antes de migrar");
    }
    MIGRACIONES.run(pool).await.context("no pude aplicar las migraciones")?;
    tracing::info!(?pendientes, "migraciones aplicadas");
    Ok(())
}

/// Deja al día el índice de palabras: lo arma de nuevo si le faltan textos
/// (por ejemplo, después de traer datos de otra base) o si cambió la forma
/// de sacar las raíces de las palabras.
pub async fn textos_al_dia(pool: &SqlitePool) -> anyhow::Result<()> {
    let version = texto::version();
    let guardada: Option<String> =
        sqlx::query_scalar("select valor from ajustes where clave = 'raices'").fetch_optional(pool).await?;
    let (notas, notas_con_palabras, fragmentos, fragmentos_con_palabras): (i64, i64, i64, i64) = sqlx::query_as(
        "select (select count(*) from notas), (select count(*) from notas_texto_docsize),
                (select count(*) from fragmentos), (select count(*) from fragmentos_texto_docsize)",
    )
    .fetch_one(pool)
    .await?;
    let cambio = guardada.as_deref() != Some(version.as_str());
    if !cambio && notas == notas_con_palabras && fragmentos == fragmentos_con_palabras {
        return Ok(());
    }
    let mut tx = base::escribir(pool).await?;
    if cambio || notas != notas_con_palabras {
        let cuantas = notas::reindexar(&mut tx).await?;
        tracing::info!(cuantas, "armé de nuevo las palabras de las notas");
    }
    if cambio || fragmentos != fragmentos_con_palabras {
        let cuantos = lector::reindexar(&mut tx).await?;
        tracing::info!(cuantos, "armé de nuevo las palabras de los pedazos de conversación");
    }
    sqlx::query(
        "insert into ajustes (clave, valor) values ('raices', ?1)
         on conflict (clave) do update set valor = excluded.valor",
    )
    .bind(&version)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Quien va a calcular las huellas, según la configuración.
fn proveedor(cfg: &Config) -> anyhow::Result<Arc<dyn Huellas>> {
    Ok(match cfg.huellas {
        Proveedor::Interno => Arc::new(Interno::nuevo(&cfg.modelo_huellas, &cfg.modelos, cfg.hilos)?),
        Proveedor::Ollama => {
            Arc::new(huellas::Ollama::nuevo(&cfg.ollama, &cfg.modelo_huellas).context("no pude preparar las huellas")?)
        }
    })
}

/// Deja todo listo sin arrancar el servicio: la base creada y al día, y el
/// modelo de huellas bajado y probado. Es lo que corre el instalador, para
/// que la bajada del modelo se vea y no pase después, de fondo.
pub async fn preparar(cfg: &Config) -> anyhow::Result<String> {
    let pool = base::abrir(&cfg.bd).await?;
    migrar(&pool, cfg).await?;
    textos_al_dia(&pool).await?;
    pool.close().await;
    let de_la_base = format!("Base lista en {}.", cfg.bd.display());
    let de_las_huellas = preparar_huellas(cfg).await?;
    Ok(format!("{de_la_base}\n{de_las_huellas}"))
}

async fn preparar_huellas(cfg: &Config) -> anyhow::Result<String> {
    match cfg.huellas {
        Proveedor::Interno => {
            let interno = Interno::nuevo(&cfg.modelo_huellas, &cfg.modelos, cfg.hilos)?;
            let ficha = interno.ficha();
            if !interno.bajado() {
                eprintln!("Bajando el modelo de huellas: {} …", ficha.descripcion);
            }
            let tardo = interno.preparar(true).await?;
            Ok(format!(
                "Modelo de huellas listo: {} en {}. Una huella tarda {} ms con {} núcleos.",
                ficha.nombre,
                cfg.modelos.display(),
                tardo.as_millis(),
                cfg.hilos
            ))
        }
        Proveedor::Ollama => {
            let ollama = huellas::Ollama::nuevo(&cfg.ollama, &cfg.modelo_huellas)?;
            let inicio = std::time::Instant::now();
            ollama.calcular(&["hola".to_owned()], huellas::Uso::Consulta).await.with_context(|| {
                format!("Ollama no respondió en {} con el modelo {}", cfg.ollama, cfg.modelo_huellas)
            })?;
            Ok(format!(
                "Huellas por Ollama listas: {} en {} ({} ms).",
                cfg.modelo_huellas,
                cfg.ollama,
                inicio.elapsed().as_millis()
            ))
        }
    }
}

/// Lee ahora las fuentes encendidas (WhatsApp, Slack) y guarda lo que el
/// usuario escribió en ellas; con `probar`, solo cuenta lo que entraría. Es
/// lo que corre `memoria juntar`, a mano: el servicio las lee solo cuando
/// toca. Devuelve si anduvo y qué pasó.
pub async fn juntar(cfg: &Config, probar: bool) -> anyhow::Result<(bool, String)> {
    let pool = base::abrir(&cfg.bd).await?;
    let lista: i64 = sqlx::query_scalar("select count(*) from sqlite_master where name = 'mensajes_propios'")
        .fetch_one(&pool)
        .await?;
    let resultado = if lista == 0 {
        Ok((false, "La base todavía no está lista: tiene que arrancar el servicio primero.".to_owned()))
    } else if probar {
        Ok(estilo::fuentes::probar(&pool, &cfg.fuentes, &cfg.nombre).await)
    } else {
        estilo::fuentes::juntar(&pool, &cfg.fuentes, &cfg.nombre).await.map_err(Into::into)
    };
    pool.close().await;
    resultado
}

/// Arranca todo y atiende hasta que llegue la señal de apagado.
pub async fn arrancar(cfg: Config) -> anyhow::Result<()> {
    let clave = clave::cargar_o_crear(&cfg.archivo_clave())?;
    let pool = base::abrir(&cfg.bd).await?;
    migrar(&pool, &cfg).await?;
    textos_al_dia(&pool).await?;
    let huellas = proveedor(&cfg)?;
    huellas::al_arrancar(&pool, huellas.modelo()).await?;
    let mut estado = Estado::nuevo(pool.clone(), huellas);
    if let Some(umbrales) = cfg.umbrales {
        estado.umbrales = umbrales;
    }
    estado.nombre = Arc::from(cfg.nombre.as_str());
    estado.fuentes = Arc::new(cfg.fuentes.clone());
    let (notas, fragmentos) = huellas::cargar(&pool, &estado.indice).await?;
    tracing::info!(notas, fragmentos, "huellas cargadas en memoria");

    let apagado = CancellationToken::new();
    let tareas = [
        respaldo::tarea_diaria(pool.clone(), cfg.clone()),
        huellas::tarea(estado.clone(), apagado.clone()),
        lector::tarea(estado.clone(), cfg.conversaciones.clone(), apagado.clone()),
        estilo::fuentes::tarea(estado.clone(), apagado.clone()),
    ];
    let rutas = web::rutas(estado, &clave, apagado.clone());
    let escucha = tokio::net::TcpListener::bind(cfg.direccion)
        .await
        .with_context(|| format!("no pude escuchar en {}", cfg.direccion))?;
    tracing::info!(direccion = %cfg.direccion, version = env!("CARGO_PKG_VERSION"), "memoria escuchando");

    let fin = apagado.clone();
    axum::serve(escucha, rutas)
        .with_graceful_shutdown(async move {
            esperar_apagado().await;
            fin.cancel();
        })
        .await?;
    for tarea in tareas {
        tarea.abort();
    }
    pool.close().await;
    tracing::info!("memoria apagada");
    Ok(())
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
