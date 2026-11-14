//! Señales (punto 4.8 del diseño): rastros que deja el uso de la memoria,
//! para detectar fallas sin depender de que Claude las reporte.
//!
//! Tipos que se guardan: `busqueda` (cada búsqueda con resultados),
//! `busqueda_vacia`, `lenta` y `error`.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::base;
use crate::error::Resultado;
use crate::grupos::{self, Grupo};
use crate::mejoras::{self, Registrada, Reporte};
use crate::texto::{Consulta, conjunto, para_indexar};

/// Cuántas búsquedas vacías parecidas en un grupo hacen falta para proponer
/// una mejora, y hasta cuántos días atrás se miran.
const VACIAS_PARA_PROPONER: usize = 3;
const DIAS: i64 = 14;
/// Cuántas búsquedas vacías recientes se miran, como mucho.
const RECIENTES: i64 = 500;
/// Una búsqueda que tarda más que esto queda anotada como lenta.
pub const LENTA: Duration = Duration::from_secs(1);

/// Guarda una señal. Si no se puede, queda en el registro y nada más: una
/// señal perdida no tiene que frenar lo que Claude estaba haciendo.
pub async fn registrar(pool: &SqlitePool, tipo: &str, sesion: Option<&str>, detalle: Value) {
    let conversacion = match sesion {
        Some(s) => grupos::conversacion_conocida(pool, s).await.ok().flatten(),
        None => None,
    };
    let r = sqlx::query("insert into senales (tipo, conversacion, detalle, fecha) values (?1, ?2, ?3, ?4)")
        .bind(tipo)
        .bind(conversacion)
        .bind(detalle.to_string())
        .bind(base::ahora())
        .execute(pool)
        .await;
    if let Err(error) = r {
        tracing::warn!(%error, tipo, "no pude guardar la señal");
    }
}

/// Si un texto se parece a lo buscado: tiene al menos la mitad de sus palabras.
fn parecido(buscado: &Consulta, otro: &str) -> bool {
    buscado.total() > 0 && buscado.coinciden(&conjunto(&para_indexar(otro))) * 2 >= buscado.total()
}

/// Lo que ya se propuso por búsquedas como esta en el grupo: el estado de
/// cada propuesta y cuándo terminó.
async fn propuestas_parecidas(
    pool: &SqlitePool,
    buscado: &Consulta,
    grupo_id: Option<i64>,
) -> Resultado<Vec<(String, Option<DateTime<Utc>>)>> {
    let filas: Vec<(String, Option<DateTime<Utc>>, String)> = sqlx::query_as(
        "select estado, terminada, json_extract(ejemplo, '$.buscado')
         from mejoras
         where origen = 'servicio' and grupo_id is ?1 and json_extract(ejemplo, '$.buscado') is not null",
    )
    .bind(grupo_id)
    .fetch_all(pool)
    .await?;
    Ok(filas.into_iter().filter(|(_, _, otro)| parecido(buscado, otro)).map(|(e, t, _)| (e, t)).collect())
}

/// Las búsquedas vacías parecidas a esta, de la más nueva a la más vieja:
/// qué se buscó y en qué conversación.
async fn vacias_parecidas(
    pool: &SqlitePool,
    buscado: &Consulta,
    grupo: Option<&str>,
    desde: Option<DateTime<Utc>>,
) -> Resultado<Vec<(String, Option<String>)>> {
    let filas: Vec<(String, Option<String>)> = sqlx::query_as(
        "select json_extract(detalle, '$.consulta'), conversacion
         from senales
         where tipo = 'busqueda_vacia'
           and fecha > ?1
           and (?2 is null or fecha > ?2)
           and json_extract(detalle, '$.grupo') is ?3
           and json_extract(detalle, '$.consulta') is not null
         order by fecha desc
         limit ?4",
    )
    .bind(base::ahora() - chrono::Duration::days(DIAS))
    .bind(desde)
    .bind(grupo)
    .bind(RECIENTES)
    .fetch_all(pool)
    .await?;
    Ok(filas.into_iter().filter(|(otra, _)| parecido(buscado, otra)).take(10).collect())
}

/// Después de una búsqueda sin resultados (ya anotada como señal): si en el
/// mismo grupo van varias parecidas, registra una propuesta de mejora y
/// devuelve su id. «Parecidas»: comparten al menos la mitad de las palabras.
/// La misma consulta repetida en una sola conversación no cuenta.
pub async fn tras_busqueda_vacia(
    pool: &SqlitePool,
    consulta: &str,
    grupo: Option<&Grupo>,
    sesion: Option<&str>,
) -> Option<i64> {
    let nombre = grupo.map(Grupo::nombre);
    let grupo_id = grupo.map(|g| g.id);
    let buscado = Consulta::de(consulta);
    // Lo que ya se propuso por búsquedas como esta en el grupo. Si sigue
    // abierto o el usuario dijo que no, no se insiste. Si ya se trabajó (se instaló
    // o falló), solo cuentan las búsquedas vacías posteriores.
    let anteriores = propuestas_parecidas(pool, &buscado, grupo_id).await.ok()?;
    if anteriores.iter().any(|(estado, _)| !matches!(estado.as_str(), "instalada" | "fallida")) {
        return None;
    }
    let desde = anteriores.iter().filter_map(|(_, terminada)| *terminada).max();
    let parecidas = vacias_parecidas(pool, &buscado, nombre.as_deref(), desde).await.ok()?;
    if parecidas.len() < VACIAS_PARA_PROPONER {
        return None;
    }
    let consultas: HashSet<String> = parecidas.iter().map(|(c, _)| c.to_lowercase()).collect();
    let conversaciones: HashSet<&Option<String>> = parecidas.iter().map(|(_, c)| c).collect();
    if consultas.len() < 2 && conversaciones.len() < 2 {
        return None;
    }

    let donde = nombre.clone().unwrap_or_else(|| "toda la memoria".to_owned());
    let mut distintas: Vec<&str> = Vec::new();
    for (otra, _) in &parecidas {
        if !distintas.iter().any(|d| d.eq_ignore_ascii_case(otra)) {
            distintas.push(otra);
        }
    }
    let conversacion = match sesion {
        Some(s) => grupos::conversacion_conocida(pool, s).await.ok().flatten(),
        None => None,
    };
    let reporte = Reporte {
        resumen: format!(
            "En {donde} se buscó {} veces algo parecido a «{consulta}» y no apareció nada: puede faltar una nota o estar fallando la búsqueda.",
            parecidas.len()
        ),
        detalle: format!(
            "Búsquedas sin resultados en {donde} en los últimos {DIAS} días: {}. Hay que ver si lo buscado está guardado con otras palabras (y la búsqueda no lo encuentra) o si directamente no está.",
            distintas.iter().map(|d| format!("«{d}»")).collect::<Vec<_>>().join(", ")
        ),
        buscado: Some(consulta.to_owned()),
        obtenido: Some("nada".to_owned()),
        origen: "servicio",
        conversacion,
        grupo_id,
        ..Reporte::default()
    };
    match mejoras::reportar(pool, &reporte).await {
        Ok(Registrada::Nueva(id)) => {
            tracing::info!(mejora = id, consulta, grupo = %donde, "propuesta de mejora por búsquedas vacías");
            Some(id)
        }
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, "no pude registrar la propuesta de mejora");
            None
        }
    }
}
