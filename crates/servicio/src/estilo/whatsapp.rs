//! WhatsApp como fuente (punto 4.11 del diseño): lo que el usuario escribió
//! en WhatsApp Desktop, leído de la base local de la aplicación.
//!
//! Se lee una copia: la base de la aplicación nunca se abre ni se toca. La
//! copia vive unos segundos en una carpeta propia y se borra al terminar.
//! Solo entran mensajes de texto que mandó el usuario; lo que escribieron los
//! demás no se guarda nunca. Tampoco los números de teléfono: de cada chat
//! queda su nombre y una marca que no dice de quién es.
//!
//! macOS protege los datos de las otras aplicaciones y la primera vez le
//! pregunta al usuario si deja leerlos. Por eso esto no lo hace el servicio,
//! que corre de fondo y cambia de identidad con cada versión, sino
//! `memoria juntar`, que lo corre Claude Code a pedido del mod.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::idioma;
use super::propios::{self, Propio};
use crate::config::WhatsApp;
use crate::texto;

pub const FUENTE: &str = "whatsapp";
/// Con qué nombre queda anotado hasta dónde se leyó.
const CURSOR: &str = "mensajes";
/// Segundos entre 1970 y 2001, que es desde donde cuenta las fechas la aplicación.
const DESDE_2001: f64 = 978_307_200.0;
/// Cuántos mensajes se leen por vez.
const TANDA: i64 = 50_000;
/// Un texto recibido más corto que esto lo puede escribir cualquiera
/// («dale, nos vemos»): no alcanza para decir que el usuario lo reenvió.
const REENVIO_MINIMO: usize = 20;
/// Cómo terminan los nombres internos de los chats de verdad: con una
/// persona, de grupo, o con alguien cuyo número no se ve. Los estados, las
/// listas de difusión y los canales terminan de otra forma y no se leen.
const DE_CHAT: [&str; 3] = ["@s.whatsapp.net", "@g.us", "@lid"];

/// Lo que salió de leer WhatsApp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Juntado {
    /// Mensajes de texto del usuario que se miraron.
    pub leidos: usize,
    /// Los que quedaron como suyos después de filtrar.
    pub propios: usize,
    /// Los que no estaban guardados (cero si solo se estaba probando).
    pub nuevos: usize,
    /// En cuántos chats.
    pub chats: usize,
}

/// Una copia de la base de la aplicación, en una carpeta propia que se borra
/// al soltarla.
struct Copia {
    carpeta: PathBuf,
}

impl Copia {
    fn de(base: &Path) -> anyhow::Result<Self> {
        static CUENTA: AtomicU32 = AtomicU32::new(0);
        let numero = CUENTA.fetch_add(1, Ordering::Relaxed);
        let carpeta = std::env::temp_dir().join(format!("memoria-whatsapp-{}-{numero}", std::process::id()));
        let _ = std::fs::remove_dir_all(&carpeta);
        std::fs::create_dir_all(&carpeta).with_context(|| format!("no pude crear {}", carpeta.display()))?;
        // Adentro van a estar, por unos segundos, todos los chats.
        std::fs::set_permissions(&carpeta, std::fs::Permissions::from_mode(0o700))?;
        let copia = Self { carpeta };
        std::fs::copy(base, copia.archivo())
            .with_context(|| format!("no pude leer la base de WhatsApp ({})", base.display()))?;
        // Lo último que escribió la aplicación está en estos dos; si faltan, se lee lo que haya.
        for sufijo in ["-wal", "-shm"] {
            let _ = std::fs::copy(con_sufijo(base, sufijo), con_sufijo(&copia.archivo(), sufijo));
        }
        Ok(copia)
    }

    fn archivo(&self) -> PathBuf {
        self.carpeta.join("ChatStorage.sqlite")
    }
}

impl Drop for Copia {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.carpeta);
    }
}

fn con_sufijo(archivo: &Path, sufijo: &str) -> PathBuf {
    let mut nombre = archivo.as_os_str().to_owned();
    nombre.push(sufijo);
    PathBuf::from(nombre)
}

/// La fecha de un mensaje, que la aplicación guarda en segundos desde 2001.
fn fecha_de(segundos: f64) -> Option<DateTime<Utc>> {
    let milesimas = ((segundos + DESDE_2001) * 1000.0).round();
    if !milesimas.is_finite() || milesimas <= 0.0 || milesimas >= 1.0e14 {
        return None;
    }
    #[expect(clippy::cast_possible_truncation, reason = "ya se vio que entra de sobra")]
    DateTime::from_timestamp_millis(milesimas as i64)
}

/// Si es un chat con una persona o un grupo (y no un estado, una lista de
/// difusión o un canal).
fn es_chat(interno: Option<&str>) -> bool {
    interno.is_some_and(|i| DE_CHAT.iter().any(|fin| i.ends_with(fin)))
}

/// Si el chat está entre los que el usuario dejó afuera.
fn excluido(nombre: Option<&str>, sin: &[String]) -> bool {
    let nombre = idioma::plano(nombre.unwrap_or_default());
    sin.iter().map(|s| idioma::plano(s.trim())).any(|s| !s.is_empty() && nombre.contains(&s))
}

/// El nombre del chat, si es un nombre: un contacto sin agendar figura con su
/// número, y los números no se guardan.
fn nombre_del_chat(nombre: Option<&str>) -> Option<String> {
    let nombre = nombre?.trim();
    nombre.chars().any(char::is_alphabetic).then(|| nombre.to_owned())
}

/// Lo que recibió el usuario (textos de cierto largo), con la fecha de la
/// primera vez: si después mandó exactamente lo mismo, lo reenvió o lo copió.
async fn recibidos(whatsapp: &SqlitePool) -> sqlx::Result<HashMap<String, f64>> {
    let filas: Vec<(String, f64)> = sqlx::query_as(
        "select cast(ZTEXT as text), coalesce(cast(ZMESSAGEDATE as real), 0.0) from ZWAMESSAGE
         where ZISFROMME = 0 and ZMESSAGETYPE = 0 and ZTEXT is not null and length(ZTEXT) >= ?1",
    )
    .bind(i64::try_from(REENVIO_MINIMO).unwrap_or(i64::MAX))
    .fetch_all(whatsapp)
    .await?;
    let mut primeras: HashMap<String, f64> = HashMap::with_capacity(filas.len());
    for (texto, fecha) in filas {
        primeras.entry(texto::marca(texto.trim())).and_modify(|antes| *antes = antes.min(fecha)).or_insert(fecha);
    }
    Ok(primeras)
}

type Fila = (i64, String, f64, Option<String>, Option<String>);

/// Los mensajes de texto que mandó el usuario, desde el que sigue a `desde`.
async fn mandados(whatsapp: &SqlitePool, desde: i64) -> sqlx::Result<Vec<Fila>> {
    sqlx::query_as(
        "select cast(m.Z_PK as integer), cast(m.ZTEXT as text), coalesce(cast(m.ZMESSAGEDATE as real), 0.0),
                cast(c.ZPARTNERNAME as text), cast(c.ZCONTACTJID as text)
         from ZWAMESSAGE m join ZWACHATSESSION c on c.Z_PK = m.ZCHATSESSION
         where m.ZISFROMME = 1 and m.ZMESSAGETYPE = 0 and m.ZTEXT is not null and m.ZTEXT <> '' and m.Z_PK > ?1
         order by m.Z_PK limit ?2",
    )
    .bind(desde)
    .bind(TANDA)
    .fetch_all(whatsapp)
    .await
}

/// Los mensajes de un chat que quedaron como propios.
#[derive(Default)]
struct DelChat {
    para: Option<String>,
    mensajes: Vec<Propio>,
}

async fn juntar_de(
    pool: &SqlitePool,
    whatsapp: &SqlitePool,
    cfg: &WhatsApp,
    nombre: &str,
    anotar: bool,
) -> anyhow::Result<Juntado> {
    let leido: Option<i64> = sqlx::query_scalar("select hasta from lecturas_propias where fuente = ?1 and origen = ?2")
        .bind(FUENTE)
        .bind(CURSOR)
        .fetch_optional(pool)
        .await?;
    let mut ultimo = leido.unwrap_or(0);
    let recibidos = recibidos(whatsapp).await.context("la base de WhatsApp no tiene la forma esperada")?;
    let mut juntado = Juntado::default();
    let mut por_chat: HashMap<String, DelChat> = HashMap::new();
    loop {
        let tanda = mandados(whatsapp, ultimo).await.context("la base de WhatsApp no tiene la forma esperada")?;
        let Some((mayor, ..)) = tanda.last() else { break };
        ultimo = *mayor;
        for (_, texto, segundos, chat, interno) in &tanda {
            if !es_chat(interno.as_deref()) || excluido(chat.as_deref(), &cfg.sin) {
                continue;
            }
            juntado.leidos += 1;
            let texto = texto.trim();
            let reenviado = texto.chars().count() >= REENVIO_MINIMO
                && recibidos.get(&texto::marca(texto)).is_some_and(|antes| antes < segundos);
            if reenviado {
                continue;
            }
            let (Some(propio), Some(fecha)) = (propios::lo_propio(texto, nombre), fecha_de(*segundos)) else {
                continue;
            };
            // Del chat queda una marca, no su nombre interno: ahí va el número de teléfono.
            let clave = format!("wa:{}", &texto::marca(interno.as_deref().unwrap_or_default())[..12]);
            let del_chat = por_chat.entry(clave).or_default();
            del_chat.para = del_chat.para.take().or_else(|| nombre_del_chat(chat.as_deref()));
            del_chat.mensajes.push(Propio { texto: propio, fecha });
            juntado.propios += 1;
        }
    }
    juntado.chats = por_chat.len();
    if !anotar {
        return Ok(juntado);
    }
    for (clave, del_chat) in &por_chat {
        juntado.nuevos +=
            super::guardar(pool, FUENTE, FUENTE, Some(clave), del_chat.para.as_deref(), &del_chat.mensajes).await?;
    }
    sqlx::query(
        "insert into lecturas_propias (fuente, origen, hasta, tamano) values (?1, ?2, ?3, 0)
         on conflict (fuente, origen) do update set hasta = excluded.hasta",
    )
    .bind(FUENTE)
    .bind(CURSOR)
    .bind(ultimo)
    .execute(pool)
    .await?;
    Ok(juntado)
}

/// Lee lo que el usuario (`nombre`) mandó por WhatsApp desde la última vez y,
/// con `anotar`, lo guarda en `pool`. Sin `anotar` solo cuenta: sirve para
/// ver qué entraría antes de encender la fuente.
pub async fn juntar(pool: &SqlitePool, cfg: &WhatsApp, nombre: &str, anotar: bool) -> anyhow::Result<Juntado> {
    let base = cfg.base.clone();
    let copia = tokio::task::spawn_blocking(move || Copia::de(&base)).await.context("se cortó la copia")??;
    let whatsapp = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(copia.archivo()))
        .await
        .context("no pude abrir la copia de la base de WhatsApp")?;
    let resultado = juntar_de(pool, &whatsapp, cfg, nombre, anotar).await;
    whatsapp.close().await;
    drop(copia);
    resultado
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn solo_se_leen_los_chats_de_verdad() {
        assert!(es_chat(Some("5491100000000@s.whatsapp.net")));
        assert!(es_chat(Some("120363000000000000@g.us")));
        assert!(es_chat(Some("100000000000000@lid")));
        assert!(!es_chat(Some("status@broadcast")));
        assert!(!es_chat(Some("120363000000000000@newsletter")));
        assert!(!es_chat(None));
    }

    #[test]
    fn deja_afuera_los_chats_que_eligio_el_usuario() {
        let sin = vec!["Familia".to_owned(), " los del cole ".to_owned(), String::new()];
        assert!(excluido(Some("FAMILIA Pérez 👪"), &sin));
        assert!(excluido(Some("Mamis: Los del Cole 2026"), &sin));
        assert!(!excluido(Some("Ana Trabajo"), &sin));
        assert!(!excluido(None, &sin));
        assert!(!excluido(Some("Familia"), &[]));
    }

    #[test]
    fn del_chat_queda_el_nombre_pero_no_el_numero() {
        assert_eq!(nombre_del_chat(Some(" Ana Trabajo ")).as_deref(), Some("Ana Trabajo"));
        assert_eq!(nombre_del_chat(Some("+54 9 11 0000-0000")), None);
        assert_eq!(nombre_del_chat(None), None);
    }

    #[test]
    fn lee_las_fechas_que_cuentan_desde_2001() {
        // 2026-11-12T18:27:01.745Z
        let fecha = fecha_de(816_200_821.745).unwrap();
        assert_eq!(fecha.to_rfc3339(), "2026-11-12T18:27:01.745+00:00");
        assert_eq!(fecha_de(f64::NAN), None);
        assert_eq!(fecha_de(-978_307_300.0), None);
        assert_eq!(fecha_de(1.0e30), None);
    }
}
