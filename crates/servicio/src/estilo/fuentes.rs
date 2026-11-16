//! Las fuentes optativas (hoy, WhatsApp): cuándo toca leerlas y qué pasó la
//! última vez.
//!
//! El servicio no las lee: le avisa al mod que toca, y el mod corre
//! `memoria juntar` desde Claude Code (ver `whatsapp.rs`). Lo que pasó queda
//! anotado en la base, así el servicio sabe cuándo volver a pedirlo: cada
//! unas horas si anduvo, y una vez por día si no, para no insistir con un
//! permiso que el usuario todavía no dio.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::whatsapp;
use crate::base;
use crate::config::Fuentes;
use crate::error::Resultado;

/// Con qué clave queda anotada la última juntada en `ajustes`.
const CLAVE: &str = "juntada";
/// Cada cuántas horas se vuelve a juntar si la última vez anduvo…
const HORAS_SI_ANDUVO: i64 = 6;
/// …y si no anduvo.
const HORAS_SI_FALLO: i64 = 24;

/// Lo que pasó la última vez que se leyeron las fuentes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Juntada {
    pub cuando: DateTime<Utc>,
    pub bien: bool,
    /// Qué pasó, dicho para el usuario.
    pub resumen: String,
}

/// Cómo se pide una juntada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modo {
    /// Solo si ya pasó el tiempo desde la anterior (lo que pide el mod).
    SiToca,
    /// Ahora, haya pasado o no.
    Ya,
    /// Sin guardar nada: cuenta qué entraría.
    Probar,
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

async fn anotar<'e, E>(conexion: E, juntada: &Juntada) -> Resultado<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        "insert into ajustes (clave, valor) values (?1, ?2)
         on conflict (clave) do update set valor = excluded.valor",
    )
    .bind(CLAVE)
    .bind(serde_json::to_string(juntada).unwrap_or_default())
    .execute(conexion)
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
        "macOS no dejó leer los datos de WhatsApp. Se permite en Ajustes del Sistema › Privacidad y seguridad, \
         dándole acceso a Claude Code a los datos de otras aplicaciones."
            .to_owned()
    } else {
        format!("{falla:#}")
    }
}

/// Lee las fuentes encendidas y guarda lo que el usuario (`nombre`) escribió
/// en ellas. Devuelve si anduvo y qué pasó, dicho para el usuario.
pub async fn juntar(pool: &SqlitePool, fuentes: &Fuentes, nombre: &str, modo: Modo) -> anyhow::Result<(bool, String)> {
    let Some(cfg) = &fuentes.whatsapp else {
        return Ok((true, "No hay ninguna fuente encendida (MEMORIA_FUENTES): no hay nada que juntar.".to_owned()));
    };
    if modo == Modo::Probar {
        return Ok(match whatsapp::juntar(pool, cfg, nombre, false).await {
            Ok(j) => (
                true,
                format!(
                    "WhatsApp: entrarían {} mensajes tuyos de {} chats (de {} mensajes de texto que mandaste). No \
                     guardé nada.",
                    j.propios, j.chats, j.leidos
                ),
            ),
            Err(falla) => (false, format!("WhatsApp: no se pudo leer. {}", motivo(&falla))),
        });
    }
    // Se anota antes de empezar: si dos conversaciones lo piden a la vez, lo
    // hace una sola, y si queda a medias no se insiste enseguida.
    let mut tx = base::escribir(pool).await?;
    let anterior: Option<String> =
        sqlx::query_scalar("select valor from ajustes where clave = ?1").bind(CLAVE).fetch_optional(&mut *tx).await?;
    let anterior: Option<Juntada> = anterior.and_then(|v| serde_json::from_str(&v).ok());
    if modo == Modo::SiToca && !vencida(anterior.as_ref(), base::ahora()) {
        return Ok((true, "Todavía no toca juntar de nuevo.".to_owned()));
    }
    let empezada = Juntada { cuando: base::ahora(), bien: false, resumen: "Empezó y no terminó.".to_owned() };
    anotar(&mut *tx, &empezada).await?;
    tx.commit().await?;

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
        assert!(motivo(&negado).contains("Privacidad y seguridad"), "{}", motivo(&negado));
        let otra = anyhow::anyhow!("la base de WhatsApp no tiene la forma esperada");
        assert_eq!(motivo(&otra), "la base de WhatsApp no tiene la forma esperada");
    }
}
