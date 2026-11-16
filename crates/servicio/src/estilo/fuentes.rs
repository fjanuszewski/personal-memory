//! Las fuentes optativas (hoy, WhatsApp): cuándo se leen y qué pasó la última
//! vez.
//!
//! Las lee el servicio, solo, cada unas horas ([`tarea`]). macOS puede frenar
//! la lectura hasta que el usuario conteste un cartel, y ese permiso dura lo
//! que vive el programa que lo pidió: por eso lee el servicio, que vive días,
//! y no cada conversación de Claude. Mientras el cartel está sin contestar la
//! lectura queda esperando y no se pide otra. Lo que pasó queda anotado en la
//! base: si anduvo se vuelve a leer en unas horas; si no, una vez por día y
//! cada vez que arranca el servicio, para no insistir con un permiso que el
//! usuario no dio. `memoria juntar` hace lo mismo a mano.

use std::sync::atomic::Ordering;
use std::time::Duration as Rato;

use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::whatsapp;
use crate::base;
use crate::config::Fuentes;
use crate::error::Resultado;
use crate::estado::Estado;

/// Con qué clave queda anotada la última juntada en `ajustes`.
const CLAVE: &str = "juntada";
/// Cada cuántas horas se vuelve a juntar si la última vez anduvo…
const HORAS_SI_ANDUVO: i64 = 6;
/// …y si no anduvo.
const HORAS_SI_FALLO: i64 = 24;
/// Cuánto espera el servicio, recién arrancado, antes de leer por primera vez…
const AL_ARRANCAR: Rato = Rato::from_secs(60);
/// …y cada cuánto mira después si ya toca.
const CADA: Rato = Rato::from_secs(600);
/// Desde cuántos minutos de espera se avisa que la lectura está frenada.
const MINUTOS_PARA_AVISAR: i64 = 2;

/// Lo que pasó la última vez que se leyeron las fuentes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Juntada {
    pub cuando: DateTime<Utc>,
    pub bien: bool,
    /// Qué pasó, dicho para el usuario.
    pub resumen: String,
}

/// La última juntada, si hubo alguna.
pub async fn ultima(pool: &SqlitePool) -> Resultado<Option<Juntada>> {
    let valor: Option<String> =
        sqlx::query_scalar("select valor from ajustes where clave = ?1").bind(CLAVE).fetch_optional(pool).await?;
    Ok(valor.and_then(|v| serde_json::from_str(&v).ok()))
}

fn vencida(ultima: Option<&Juntada>, ahora: DateTime<Utc>) -> bool {
    ultima.is_none_or(|j| ahora - j.cuando >= Duration::hours(if j.bien { HORAS_SI_ANDUVO } else { HORAS_SI_FALLO }))
}

/// Si hay alguna fuente encendida y ya pasó el tiempo desde la última juntada.
pub async fn toca(pool: &SqlitePool, fuentes: &Fuentes) -> Resultado<bool> {
    Ok(fuentes.alguna() && vencida(ultima(pool).await?.as_ref(), base::ahora()))
}

async fn anotar(pool: &SqlitePool, juntada: &Juntada) -> Resultado<()> {
    sqlx::query(
        "insert into ajustes (clave, valor) values (?1, ?2)
         on conflict (clave) do update set valor = excluded.valor",
    )
    .bind(CLAVE)
    .bind(serde_json::to_string(juntada).unwrap_or_default())
    .execute(pool)
    .await?;
    Ok(())
}

/// Por qué no se pudo leer, dicho para el usuario. Si fue el sistema el que
/// no dejó, dice cómo se arregla.
fn motivo(falla: &anyhow::Error) -> String {
    let sin_permiso = falla.chain().any(|causa| {
        causa.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
    });
    if sin_permiso {
        "macOS no dejó leer los datos de WhatsApp. Cuando pregunte si deja acceder a los datos de otras apps, hay \
         que tocar «Permitir». Se vuelve a intentar una vez por día y cada vez que arranca el servicio."
            .to_owned()
    } else {
        format!("{falla:#}")
    }
}

/// Cuenta lo que entraría de las fuentes encendidas, sin guardar nada.
/// Devuelve si se pudo leer y qué pasó, dicho para el usuario.
pub async fn probar(pool: &SqlitePool, fuentes: &Fuentes, nombre: &str) -> (bool, String) {
    let Some(cfg) = &fuentes.whatsapp else {
        return (true, SIN_FUENTES.to_owned());
    };
    match whatsapp::juntar(pool, cfg, nombre, false).await {
        Ok(j) => (
            true,
            format!(
                "WhatsApp: entrarían {} mensajes tuyos de {} chats (de {} mensajes de texto que mandaste). No guardé \
                 nada.",
                j.propios, j.chats, j.leidos
            ),
        ),
        Err(falla) => (false, format!("WhatsApp: no se pudo leer. {}", motivo(&falla))),
    }
}

const SIN_FUENTES: &str = "No hay ninguna fuente encendida (MEMORIA_FUENTES): no hay nada que juntar.";

/// Lee las fuentes encendidas y guarda lo que el usuario (`nombre`) escribió
/// en ellas. Devuelve si anduvo y qué pasó, dicho para el usuario; eso mismo
/// queda anotado, y de ahí sale cuándo toca la próxima.
pub async fn juntar(pool: &SqlitePool, fuentes: &Fuentes, nombre: &str) -> Resultado<(bool, String)> {
    let Some(cfg) = &fuentes.whatsapp else {
        return Ok((true, SIN_FUENTES.to_owned()));
    };
    let (bien, resumen) = match whatsapp::juntar(pool, cfg, nombre, true).await {
        Ok(j) => (
            true,
            format!(
                "WhatsApp: {} mensajes nuevos (de {} tuyos en {} chats que había para leer).",
                j.nuevos, j.propios, j.chats
            ),
        ),
        Err(falla) => (false, format!("WhatsApp: no se pudo leer. {}", motivo(&falla))),
    };
    anotar(pool, &Juntada { cuando: base::ahora(), bien, resumen: resumen.clone() }).await?;
    Ok((bien, resumen))
}

/// Si al servicio le toca leer ahora. Recién arrancado vuelve a probar aunque
/// la última vez no haya andado: el permiso de macOS es de cada arranque.
async fn le_toca(e: &Estado, recien_arrancado: bool) -> Resultado<bool> {
    let ultima = ultima(&e.pool).await?;
    Ok(vencida(ultima.as_ref(), base::ahora()) || (recien_arrancado && ultima.is_some_and(|j| !j.bien)))
}

/// La tarea de fondo que lee las fuentes encendidas cuando toca. Sin ninguna
/// encendida no hace nada.
#[must_use]
pub fn tarea(e: Estado, apagado: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        if !e.fuentes.alguna() {
            return;
        }
        let mut recien_arrancado = true;
        loop {
            tokio::select! {
                () = apagado.cancelled() => break,
                () = tokio::time::sleep(if recien_arrancado { AL_ARRANCAR } else { CADA }) => {},
            }
            let toca = le_toca(&e, recien_arrancado).await;
            recien_arrancado = false;
            match toca {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    tracing::warn!(%error, "no pude ver si toca leer las fuentes");
                    continue;
                }
            }
            // Puede quedar acá un buen rato: hasta que el usuario le conteste a macOS.
            e.leyendo_fuentes.store(base::ahora().timestamp(), Ordering::Relaxed);
            let resultado = juntar(&e.pool, &e.fuentes, &e.nombre).await;
            e.leyendo_fuentes.store(0, Ordering::Relaxed);
            match resultado {
                Ok((true, resumen)) => tracing::info!(resumen, "fuentes leídas"),
                Ok((false, resumen)) => tracing::warn!(resumen, "no pude leer las fuentes"),
                Err(error) => tracing::warn!(%error, "falla al leer las fuentes"),
            }
        }
    })
}

/// Lo que conviene que sepa quien pregunta cómo escribe el usuario: que hay
/// una fuente encendida esperando su permiso, o que no se pudo leer.
pub async fn aviso(e: &Estado) -> Resultado<Option<String>> {
    if !e.fuentes.alguna() {
        return Ok(None);
    }
    let desde = e.leyendo_fuentes.load(Ordering::Relaxed);
    if desde > 0 {
        let minutos = (base::ahora().timestamp() - desde) / 60;
        return Ok((minutos >= MINUTOS_PARA_AVISAR).then(|| {
            format!(
                "WhatsApp está encendido, pero la lectura espera hace {minutos} min: lo más probable es que macOS le \
                 esté mostrando al usuario, en su Mac, un cartel que pregunta si «memoria» puede acceder a los datos \
                 de otras apps. Hasta que toque «Permitir» no entra nada nuevo de WhatsApp. Decíselo."
            )
        }));
    }
    Ok(ultima(&e.pool).await?.filter(|j| !j.bien).map(|j| {
        format!(
            "WhatsApp está encendido, pero la última lectura ({}) no anduvo. {} Decíselo al usuario.",
            j.cuando.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
            j.resumen
        )
    }))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn hace(horas: i64, bien: bool) -> Juntada {
        Juntada { cuando: base::ahora() - Duration::hours(horas), bien, resumen: String::new() }
    }

    #[test]
    fn vuelve_a_tocar_segun_como_salio_la_ultima() {
        let ahora = base::ahora();
        assert!(vencida(None, ahora), "nunca se juntó");
        assert!(!vencida(Some(&hace(5, true)), ahora));
        assert!(vencida(Some(&hace(7, true)), ahora));
        // Si falló, no se insiste hasta el otro día.
        assert!(!vencida(Some(&hace(7, false)), ahora));
        assert!(vencida(Some(&hace(25, false)), ahora));
    }

    #[test]
    fn si_el_sistema_no_deja_dice_como_se_arregla() {
        let negado = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .context("no pude leer la base de WhatsApp");
        assert!(motivo(&negado).contains("«Permitir»"), "{}", motivo(&negado));
        let otra = anyhow::anyhow!("la base de WhatsApp no tiene la forma esperada");
        assert_eq!(motivo(&otra), "la base de WhatsApp no tiene la forma esperada");
    }
}
