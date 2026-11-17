//! Las fuentes optativas (WhatsApp y Slack): cuándo se leen y qué pasó la
//! última vez con cada una.
//!
//! Las lee el servicio, solo, cada unas horas ([`tarea`]), y cada una lleva su
//! cuenta: que una no ande no frena a la otra. Lo que pasó queda anotado en
//! la base: si anduvo se vuelve a leer en unas horas; si no, una vez por día y
//! cada vez que arranca el servicio. `memoria juntar` hace lo mismo a mano.
//!
//! Con WhatsApp, macOS puede frenar la lectura hasta que el usuario conteste
//! un cartel, y ese permiso dura lo que vive el programa que lo pidió: por eso
//! lee el servicio, que vive días, y no cada conversación de Claude. Mientras
//! el cartel está sin contestar la lectura queda esperando y no se pide otra;
//! y si no anduvo no se insiste hasta el otro día, para no pedir de nuevo un
//! permiso que el usuario no dio.

use std::fmt::Write as _;
use std::sync::atomic::Ordering;
use std::time::Duration as Rato;

use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{idioma, slack, whatsapp};
use crate::base;
use crate::config::Fuentes;
use crate::error::Resultado;
use crate::estado::Estado;

/// Cada cuántas horas se vuelve a leer una fuente si la última vez anduvo…
const HORAS_SI_ANDUVO: i64 = 6;
/// …y si no anduvo.
const HORAS_SI_FALLO: i64 = 24;
/// Cuánto espera el servicio, recién arrancado, antes de leer por primera vez…
const AL_ARRANCAR: Rato = Rato::from_secs(60);
/// …y cada cuánto mira después si ya toca.
const CADA: Rato = Rato::from_secs(600);
/// Desde cuántos minutos de espera se avisa que la lectura está frenada.
const MINUTOS_PARA_AVISAR: i64 = 2;

/// Una fuente optativa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fuente {
    WhatsApp,
    Slack,
}

impl Fuente {
    const TODAS: [Self; 2] = [Self::WhatsApp, Self::Slack];

    /// Cómo se le nombra al usuario.
    #[must_use]
    pub const fn nombre(self) -> &'static str {
        match self {
            Self::WhatsApp => "WhatsApp",
            Self::Slack => "Slack",
        }
    }

    /// Con qué clave queda anotada su última lectura en `ajustes`.
    const fn clave(self) -> &'static str {
        match self {
            Self::WhatsApp => "juntada:whatsapp",
            Self::Slack => "juntada:slack",
        }
    }

    const fn encendida(self, fuentes: &Fuentes) -> bool {
        match self {
            Self::WhatsApp => fuentes.whatsapp.is_some(),
            Self::Slack => fuentes.slack.is_some(),
        }
    }
}

/// Las fuentes que el usuario encendió.
fn encendidas(fuentes: &Fuentes) -> impl Iterator<Item = Fuente> + '_ {
    Fuente::TODAS.into_iter().filter(move |fuente| fuente.encendida(fuentes))
}

/// Si una conversación está entre las que el usuario dejó afuera: las que
/// tienen alguno de esos textos en el nombre.
pub(super) fn excluido(nombre: Option<&str>, sin: &[String]) -> bool {
    let nombre = idioma::plano(nombre.unwrap_or_default());
    sin.iter().map(|s| idioma::plano(s.trim())).any(|s| !s.is_empty() && nombre.contains(&s))
}

/// Lo que pasó la última vez que se leyó una fuente.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Juntada {
    pub cuando: DateTime<Utc>,
    pub bien: bool,
    /// Qué pasó, dicho para el usuario.
    pub resumen: String,
}

/// La última lectura de una fuente, si hubo alguna.
pub async fn ultima(pool: &SqlitePool, fuente: Fuente) -> Resultado<Option<Juntada>> {
    let valor: Option<String> = sqlx::query_scalar("select valor from ajustes where clave = ?1")
        .bind(fuente.clave())
        .fetch_optional(pool)
        .await?;
    Ok(valor.and_then(|v| serde_json::from_str(&v).ok()))
}

fn vencida(ultima: Option<&Juntada>, ahora: DateTime<Utc>) -> bool {
    ultima.is_none_or(|j| ahora - j.cuando >= Duration::hours(if j.bien { HORAS_SI_ANDUVO } else { HORAS_SI_FALLO }))
}

/// Si a alguna fuente encendida ya le pasó el tiempo desde su última lectura.
pub async fn toca(pool: &SqlitePool, fuentes: &Fuentes) -> Resultado<bool> {
    let ahora = base::ahora();
    for fuente in encendidas(fuentes) {
        if vencida(ultima(pool, fuente).await?.as_ref(), ahora) {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn anotar(pool: &SqlitePool, fuente: Fuente, juntada: &Juntada) -> Resultado<()> {
    sqlx::query(
        "insert into ajustes (clave, valor) values (?1, ?2)
         on conflict (clave) do update set valor = excluded.valor",
    )
    .bind(fuente.clave())
    .bind(serde_json::to_string(juntada).unwrap_or_default())
    .execute(pool)
    .await?;
    Ok(())
}

/// Por qué no se pudo leer WhatsApp, dicho para el usuario. Si fue el sistema
/// el que no dejó, dice cómo se arregla.
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

/// Lo que pasó al leer Slack, dicho para el usuario. Anduvo si se pudieron
/// leer todos los espacios; del que no, dice por qué.
fn de_slack(lectura: &slack::Lectura, anotar: bool) -> (bool, String) {
    let j = lectura.juntado;
    let espacios =
        if lectura.espacios == 1 { "1 espacio".to_owned() } else { format!("{} espacios", lectura.espacios) };
    let mut resumen = if lectura.espacios == 0 {
        "Slack:".to_owned()
    } else if anotar {
        format!(
            "Slack: {} mensajes nuevos (de {} tuyos en {} conversaciones de {espacios} que había para leer).",
            j.nuevos, j.propios, j.chats
        )
    } else {
        format!("Slack: hay {} mensajes tuyos para leer en {espacios}. No guardé nada.", j.leidos)
    };
    for falla in &lectura.fallas {
        let _ = write!(resumen, " No se pudo leer {falla}.");
    }
    (lectura.fallas.is_empty(), resumen)
}

/// Lee una fuente; con `anotar` guarda lo que el usuario (`nombre`) escribió
/// en ella, y sin `anotar` solo cuenta lo que entraría. Devuelve si se pudo
/// leer y qué pasó, dicho para el usuario.
async fn leer(pool: &SqlitePool, fuentes: &Fuentes, nombre: &str, fuente: Fuente, anotar: bool) -> (bool, String) {
    match fuente {
        Fuente::WhatsApp => {
            let Some(cfg) = &fuentes.whatsapp else { return (true, String::new()) };
            match whatsapp::juntar(pool, cfg, nombre, anotar).await {
                Ok(j) if anotar => (
                    true,
                    format!(
                        "WhatsApp: {} mensajes nuevos (de {} tuyos en {} chats que había para leer).",
                        j.nuevos, j.propios, j.chats
                    ),
                ),
                Ok(j) => (
                    true,
                    format!(
                        "WhatsApp: entrarían {} mensajes tuyos de {} chats (de {} mensajes de texto que mandaste). \
                         No guardé nada.",
                        j.propios, j.chats, j.leidos
                    ),
                ),
                Err(falla) => (false, format!("WhatsApp: no se pudo leer. {}", motivo(&falla))),
            }
        }
        Fuente::Slack => {
            let Some(cfg) = &fuentes.slack else { return (true, String::new()) };
            de_slack(&slack::juntar(pool, cfg, nombre, anotar).await, anotar)
        }
    }
}

const SIN_FUENTES: &str = "No hay ninguna fuente encendida (MEMORIA_FUENTES): no hay nada que juntar.";

/// Cuenta lo que entraría de las fuentes encendidas, sin guardar nada.
/// Devuelve si se pudieron leer todas y qué pasó, dicho para el usuario.
pub async fn probar(pool: &SqlitePool, fuentes: &Fuentes, nombre: &str) -> (bool, String) {
    let mut bien = true;
    let mut renglones: Vec<String> = Vec::new();
    for fuente in encendidas(fuentes) {
        let (anduvo, resumen) = leer(pool, fuentes, nombre, fuente, false).await;
        bien &= anduvo;
        renglones.push(resumen);
    }
    if renglones.is_empty() { (true, SIN_FUENTES.to_owned()) } else { (bien, renglones.join("\n")) }
}

/// Lee una fuente, guarda lo que el usuario escribió en ella y deja anotado
/// qué pasó: de ahí sale cuándo le toca la próxima.
async fn juntar_una(pool: &SqlitePool, fuentes: &Fuentes, nombre: &str, fuente: Fuente) -> Resultado<(bool, String)> {
    let (bien, resumen) = leer(pool, fuentes, nombre, fuente, true).await;
    anotar(pool, fuente, &Juntada { cuando: base::ahora(), bien, resumen: resumen.clone() }).await?;
    Ok((bien, resumen))
}

/// Lee ahora todas las fuentes encendidas y guarda lo que el usuario
/// (`nombre`) escribió en ellas. Devuelve si anduvieron todas y qué pasó con
/// cada una, dicho para el usuario.
pub async fn juntar(pool: &SqlitePool, fuentes: &Fuentes, nombre: &str) -> Resultado<(bool, String)> {
    let mut bien = true;
    let mut renglones: Vec<String> = Vec::new();
    for fuente in encendidas(fuentes) {
        let (anduvo, resumen) = juntar_una(pool, fuentes, nombre, fuente).await?;
        bien &= anduvo;
        renglones.push(resumen);
    }
    Ok(if renglones.is_empty() { (true, SIN_FUENTES.to_owned()) } else { (bien, renglones.join("\n")) })
}

/// Si al servicio le toca leer ahora una fuente. Recién arrancado vuelve a
/// probar aunque la última vez no haya andado: el permiso de macOS es de cada
/// arranque, y el llavero pudo estar cerrado.
async fn le_toca(e: &Estado, fuente: Fuente, recien_arrancado: bool) -> Resultado<bool> {
    let ultima = ultima(&e.pool, fuente).await?;
    Ok(vencida(ultima.as_ref(), base::ahora()) || (recien_arrancado && ultima.is_some_and(|j| !j.bien)))
}

/// Lee una fuente si le toca, y deja en el registro qué pasó.
async fn leer_si_toca(e: &Estado, fuente: Fuente, recien_arrancado: bool) {
    match le_toca(e, fuente, recien_arrancado).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::warn!(fuente = fuente.nombre(), %error, "no pude ver si toca leer la fuente");
            return;
        }
    }
    // WhatsApp puede quedar acá un buen rato: hasta que el usuario le conteste a macOS.
    let puede_esperar = fuente == Fuente::WhatsApp;
    if puede_esperar {
        e.leyendo_whatsapp.store(base::ahora().timestamp(), Ordering::Relaxed);
    }
    let resultado = juntar_una(&e.pool, &e.fuentes, &e.nombre, fuente).await;
    if puede_esperar {
        e.leyendo_whatsapp.store(0, Ordering::Relaxed);
    }
    match resultado {
        Ok((true, resumen)) => tracing::info!(resumen, "fuente leída"),
        Ok((false, resumen)) => tracing::warn!(resumen, "no pude leer la fuente"),
        Err(error) => tracing::warn!(fuente = fuente.nombre(), %error, "falla al leer la fuente"),
    }
}

/// La tarea de fondo que lee cada fuente encendida cuando le toca. Sin
/// ninguna encendida no hace nada.
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
            for fuente in encendidas(&e.fuentes) {
                leer_si_toca(&e, fuente, recien_arrancado).await;
            }
            recien_arrancado = false;
        }
    })
}

/// Lo que conviene que sepa quien pregunta cómo escribe el usuario: que hay
/// una fuente encendida esperando su permiso, o que no se pudo leer.
pub async fn aviso(e: &Estado) -> Resultado<Option<String>> {
    let mut avisos: Vec<String> = Vec::new();
    for fuente in encendidas(&e.fuentes) {
        let desde = if fuente == Fuente::WhatsApp { e.leyendo_whatsapp.load(Ordering::Relaxed) } else { 0 };
        if desde > 0 {
            let minutos = (base::ahora().timestamp() - desde) / 60;
            if minutos >= MINUTOS_PARA_AVISAR {
                avisos.push(format!(
                    "WhatsApp está encendido, pero la lectura espera hace {minutos} min: lo más probable es que \
                     macOS le esté mostrando al usuario, en su Mac, un cartel que pregunta si «memoria» puede \
                     acceder a los datos de otras apps. Hasta que toque «Permitir» no entra nada nuevo de WhatsApp. \
                     Decíselo."
                ));
            }
            continue;
        }
        if let Some(j) = ultima(&e.pool, fuente).await?.filter(|j| !j.bien) {
            avisos.push(format!(
                "{} está encendido, pero la última lectura ({}) no anduvo. {} Decíselo al usuario.",
                fuente.nombre(),
                j.cuando.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
                j.resumen
            ));
        }
    }
    Ok((!avisos.is_empty()).then(|| avisos.join("\n")))
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

    #[test]
    fn deja_afuera_las_conversaciones_que_eligio_el_usuario() {
        let sin = vec!["Familia".to_owned(), " los del cole ".to_owned(), String::new()];
        assert!(excluido(Some("FAMILIA Pérez 👪"), &sin));
        assert!(excluido(Some("Mamis: Los del Cole 2026"), &sin));
        assert!(!excluido(Some("Ana Trabajo"), &sin));
        assert!(!excluido(None, &sin));
        assert!(!excluido(Some("Familia"), &[]));
    }

    #[test]
    fn cada_fuente_se_enciende_y_se_anota_aparte() {
        assert_eq!(encendidas(&Fuentes::default()).count(), 0);
        assert_ne!(Fuente::WhatsApp.clave(), Fuente::Slack.clave());
        assert_eq!((Fuente::WhatsApp.nombre(), Fuente::Slack.nombre()), ("WhatsApp", "Slack"));
    }

    #[test]
    fn de_slack_cuenta_lo_que_entro_y_lo_que_no_se_pudo_leer() {
        let juntado = slack::Juntado { leidos: 40, propios: 30, nuevos: 12, chats: 5 };
        let todo_bien = slack::Lectura { juntado, espacios: 2, fallas: vec![] };
        assert_eq!(
            de_slack(&todo_bien, true),
            (
                true,
                "Slack: 12 mensajes nuevos (de 30 tuyos en 5 conversaciones de 2 espacios que había para leer)."
                    .to_owned()
            )
        );
        assert_eq!(
            de_slack(&todo_bien, false),
            (true, "Slack: hay 40 mensajes tuyos para leer en 2 espacios. No guardé nada.".to_owned())
        );
        // Con un espacio que no se pudo leer no anduvo, pero lo del otro queda.
        let a_medias = slack::Lectura { juntado, espacios: 1, fallas: vec!["«acme»: la llave no sirve".to_owned()] };
        let (bien, resumen) = de_slack(&a_medias, true);
        assert!(
            !bien && resumen.starts_with("Slack: 12 mensajes nuevos") && resumen.contains("de 1 espacio que"),
            "{resumen}"
        );
        assert!(resumen.ends_with(" No se pudo leer «acme»: la llave no sirve."), "{resumen}");
        let nada =
            slack::Lectura { fallas: vec!["«acme»: la llave no sirve".to_owned()], ..slack::Lectura::default() };
        assert_eq!(de_slack(&nada, true), (false, "Slack: No se pudo leer «acme»: la llave no sirve.".to_owned()));
    }
}
