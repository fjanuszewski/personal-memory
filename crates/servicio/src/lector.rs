//! Lector de conversaciones (punto 4.7 del diseño): lee lo nuevo de los
//! archivos que guarda Claude Code y lo deja en pedazos («fragmentos»), para
//! poder buscar lo conversado que nunca llegó a ser una nota.
//!
//! Solo entra lo que escribió el usuario y lo que respondió Claude. Quedan afuera
//! los resultados de herramientas, los subagentes, los avisos de la app y lo
//! pegado desde otro lado. Lee las conversaciones que registró el mod (así
//! conoce su grupo), desde donde había quedado.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::base;
use crate::error::Resultado;
use crate::estado::Estado;
use crate::indice::Clase;
use crate::notas::tapar_secretos;
use crate::{senales, texto};

/// Tamaño de cada pedazo, en caracteres, y cuánto repite del anterior.
const VENTANA: usize = 1500;
const SOLAPE: usize = 200;
/// Un mensaje del usuario más largo que esto casi siempre es algo pegado.
const LARGO_MENSAJE_USUARIO: usize = 8000;
/// Un pedazo con menos texto que esto («sí, dale») no sirve para buscar.
const MINIMO_UTIL: usize = 30;
/// Cada cuánto se revisa si hay conversaciones con algo nuevo.
const CADA: Duration = Duration::from_secs(60);
/// Una conversación se lee cuando lleva este rato sin cambios…
pub const REPOSO: Duration = Duration::from_secs(120);
/// …o cuando junta tanto sin leer que no conviene esperar más.
const MUCHO_PENDIENTE: u64 = 512 * 1024;
/// Si se leyó todo esto sin reconocer un solo mensaje, cambió el formato.
const SOSPECHOSO: u64 = 200 * 1024;

/// Bloques que la app mete dentro de los mensajes del usuario y que no escribió él.
const ETIQUETAS_DE_LA_APP: [&str; 20] = [
    "system-reminder",
    "pasted_content",
    "task-notification",
    "scheduled-task",
    "side-sessions-event",
    "ci-monitor-event",
    "local-command-caveat",
    "local-command-stdout",
    "local-command-stderr",
    "command-name",
    "command-message",
    "command-args",
    "bash-input",
    "bash-stdout",
    "bash-stderr",
    "artifact-view-context",
    "artifact-content-authored-by-others",
    "tools-used-before-summary",
    "ide_selection",
    "ide_opened_file",
];

static BLOQUES_DE_LA_APP: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    ETIQUETAS_DE_LA_APP
        .iter()
        .map(|e| Regex::new(&format!(r"(?s)<{e}\b[^>]*>.*?</{e}\b[^>]*>")).expect("patrón válido"))
        .collect()
});

static RENGLONES_VACIOS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n(\s*\n)+").expect("patrón válido"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quien {
    Usuario,
    Claude,
}

impl Quien {
    /// Cómo se lo nombra en lo que se guarda. `usuario` es el nombre de quien
    /// usa la memoria (`MEMORIA_NOMBRE`).
    const fn nombre(self, usuario: &str) -> &str {
        match self {
            Self::Usuario => usuario,
            Self::Claude => "Claude",
        }
    }
}

/// Algo que dijo el usuario o Claude, con el lugar del archivo de donde salió.
#[derive(Clone, Debug, PartialEq)]
pub struct Mensaje {
    pub quien: Quien,
    pub texto: String,
    pub desde: u64,
    pub hasta: u64,
    pub fecha: DateTime<Utc>,
}

/// Un pedazo de conversación listo para guardar.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragmento {
    pub desde: u64,
    pub hasta: u64,
    pub fecha: DateTime<Utc>,
    pub contenido: String,
}

/// Lo que salió de leer un archivo desde una posición.
#[derive(Debug, Default)]
pub struct Lectura {
    pub mensajes: Vec<Mensaje>,
    /// Hasta dónde se leyó: el final del último renglón completo.
    pub hasta: u64,
    /// Renglones del usuario o Claude que se reconocieron (aunque no dejen texto).
    pub reconocidos: usize,
    /// Renglones que parecían mensajes y no se pudieron leer.
    pub raros: usize,
}

fn contiene(renglon: &[u8], marca: &[u8]) -> bool {
    renglon.windows(marca.len()).any(|v| v == marca)
}

/// Saca de un mensaje del usuario lo que no escribió él. `None` si no queda nada.
fn limpiar(texto: &str) -> Option<String> {
    let mut limpio = texto.to_owned();
    for bloque in BLOQUES_DE_LA_APP.iter() {
        if limpio.contains('<') && bloque.is_match(&limpio) {
            limpio = bloque.replace_all(&limpio, "").into_owned();
        }
    }
    let limpio = RENGLONES_VACIOS.replace_all(limpio.trim(), "\n\n");
    if limpio.is_empty() || limpio.starts_with("[Request interrupted") {
        return None;
    }
    if limpio.chars().count() > LARGO_MENSAJE_USUARIO {
        let corte: String = limpio.chars().take(LARGO_MENSAJE_USUARIO).collect();
        return Some(format!("{corte} […]"));
    }
    Some(limpio.into_owned())
}

/// Los textos de un mensaje: el contenido puede ser un texto o una lista de
/// bloques, de los que solo importan los de texto.
fn textos(contenido: &Value) -> Option<Vec<&str>> {
    match contenido {
        Value::String(t) => Some(vec![t.as_str()]),
        Value::Array(bloques) => Some(
            bloques
                .iter()
                .filter_map(|b| match b {
                    Value::String(t) => Some(t.as_str()),
                    _ if b["type"] == "text" => b["text"].as_str(),
                    _ => None,
                })
                .collect(),
        ),
        Value::Null => Some(Vec::new()),
        _ => None,
    }
}

/// Lo que dijo alguien en un renglón del archivo. El texto queda vacío si el
/// renglón se reconoce pero no trae nada que guardar.
#[derive(Debug, PartialEq)]
struct Dicho {
    quien: Quien,
    texto: String,
    fecha: Option<DateTime<Utc>>,
}

/// El renglón parecía un mensaje y no se entendió.
#[derive(Debug)]
struct NoEntendido;

/// Qué dice un renglón del archivo, si es del usuario o de Claude.
/// `Ok(None)`: no es un mensaje.
fn mensaje_de(renglon: &[u8]) -> Result<Option<Dicho>, NoEntendido> {
    // Atajo: la mayoría de los renglones (resultados de herramientas, estado
    // de la app) no son mensajes y pueden pesar megas.
    let del_usuario = contiene(renglon, br#""type":"user""#);
    let de_claude = contiene(renglon, br#""type":"assistant""#);
    if !del_usuario && !de_claude {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(renglon).map_err(|_| NoEntendido)?;
    let quien = match v["type"].as_str() {
        Some("user") => Quien::Usuario,
        Some("assistant") => Quien::Claude,
        _ => return Ok(None),
    };
    let marcado = |clave: &str| v[clave].as_bool() == Some(true);
    let sin_texto = || Ok(Some(Dicho { quien, texto: String::new(), fecha: None }));
    if marcado("isSidechain") || marcado("isMeta") || marcado("isCompactSummary") || marcado("isApiErrorMessage") {
        return sin_texto();
    }
    let mensaje = &v["message"];
    let ajeno = match quien {
        // Avisos de tareas y mensajes de otras conversaciones entran como «user».
        Quien::Usuario => v["origin"].as_str().or_else(|| v["origin"]["kind"].as_str()).is_some_and(|o| o != "human"),
        Quien::Claude => mensaje["model"] == "<synthetic>",
    };
    if ajeno {
        return sin_texto();
    }
    let partes = textos(&mensaje["content"]).ok_or(NoEntendido)?;
    let texto = match quien {
        Quien::Usuario => partes.iter().filter_map(|t| limpiar(t)).collect::<Vec<_>>().join("\n\n"),
        Quien::Claude => partes.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n"),
    };
    let fecha = v["timestamp"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|f| f.to_utc());
    Ok(Some(Dicho { quien, texto, fecha }))
}

/// Lee el archivo desde `desde` y devuelve los mensajes nuevos. Un renglón a
/// medio escribir queda para la próxima.
pub fn leer(archivo: &Path, desde: u64) -> std::io::Result<Lectura> {
    let mut abierto = File::open(archivo)?;
    abierto.seek(SeekFrom::Start(desde))?;
    let mut entrada = BufReader::with_capacity(1 << 20, abierto);
    let mut lectura = Lectura { hasta: desde, ..Lectura::default() };
    let mut renglon = Vec::new();
    let mut ultima_fecha = None;
    loop {
        renglon.clear();
        let leidos = entrada.read_until(b'\n', &mut renglon)?;
        if leidos == 0 || renglon.last() != Some(&b'\n') {
            break;
        }
        let inicio = lectura.hasta;
        lectura.hasta += u64::try_from(leidos).unwrap_or(u64::MAX);
        match mensaje_de(&renglon) {
            Ok(Some(Dicho { quien, texto, fecha })) => {
                lectura.reconocidos += 1;
                ultima_fecha = fecha.or(ultima_fecha);
                if !texto.is_empty() {
                    let fecha = ultima_fecha.unwrap_or_else(Utc::now);
                    lectura.mensajes.push(Mensaje { quien, texto, desde: inicio, hasta: lectura.hasta, fecha });
                }
            }
            Ok(None) => {}
            Err(NoEntendido) => lectura.raros += 1,
        }
    }
    Ok(lectura)
}

/// Corta un texto largo en partes de hasta `maximo` caracteres, en lo posible
/// donde hay un espacio.
fn cortar(texto: &str, maximo: usize) -> Vec<&str> {
    let mut partes = Vec::new();
    let mut resto = texto.trim();
    while resto.chars().count() > maximo {
        let limite = resto.char_indices().nth(maximo).map_or(resto.len(), |(i, _)| i);
        let corte = resto[..limite].rfind(char::is_whitespace).filter(|i| *i > 0).unwrap_or(limite);
        partes.push(resto[..corte].trim_end());
        resto = resto[corte..].trim_start();
    }
    if !resto.is_empty() {
        partes.push(resto);
    }
    partes
}

/// El final de un texto, para repetirlo al empezar el pedazo siguiente.
fn cola(texto: &str) -> String {
    let total = texto.chars().count();
    if total <= SOLAPE {
        return texto.to_owned();
    }
    let inicio = texto.char_indices().nth(total - SOLAPE).map_or(0, |(i, _)| i);
    let desde = texto[inicio..].find(char::is_whitespace).map_or(inicio, |i| inicio + i);
    format!("…{}", &texto[desde..])
}

/// Un párrafo (o parte de uno) con el mensaje del que salió.
struct Parte {
    mensaje: usize,
    texto: String,
    /// No es el comienzo de su mensaje.
    sigue: bool,
    /// Es la cola del pedazo anterior, repetida.
    repetida: bool,
}

fn armar(partes: &[Parte], mensajes: &[Mensaje], usuario: &str) -> Option<Fragmento> {
    let primera = partes.first()?;
    let ultima = partes.last()?;
    let mut contenido = String::new();
    let mut anterior: Option<Quien> = None;
    for parte in partes {
        let quien = mensajes[parte.mensaje].quien;
        if !contenido.is_empty() {
            contenido.push_str("\n\n");
        }
        if anterior != Some(quien) {
            contenido.push_str(quien.nombre(usuario));
            contenido.push_str(if parte.sigue { " (sigue): " } else { ": " });
        }
        contenido.push_str(&parte.texto);
        anterior = Some(quien);
    }
    Some(Fragmento {
        desde: mensajes[primera.mensaje].desde,
        hasta: mensajes[ultima.mensaje].hasta,
        fecha: mensajes[primera.mensaje].fecha,
        contenido,
    })
}

/// Arma pedazos de unos [`VENTANA`] caracteres con los mensajes, en orden.
/// Cada pedazo dice quién habla (`usuario` es el nombre de quien usa la
/// memoria) y empieza repitiendo el final del anterior.
#[must_use]
pub fn fragmentar(mensajes: &[Mensaje], usuario: &str) -> Vec<Fragmento> {
    let mut partes: Vec<Parte> = Vec::new();
    for (i, mensaje) in mensajes.iter().enumerate() {
        let mut sigue = false;
        for parrafo in mensaje.texto.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
            for texto in cortar(parrafo, VENTANA) {
                partes.push(Parte { mensaje: i, texto: texto.to_owned(), sigue, repetida: false });
                sigue = true;
            }
        }
    }

    let mut fragmentos = Vec::new();
    let mut actual: Vec<Parte> = Vec::new();
    let mut largo = 0;
    let util = |partes: &[Parte]| {
        partes.iter().filter(|p| !p.repetida).map(|p| p.texto.chars().count()).sum::<usize>() >= MINIMO_UTIL
    };
    for parte in partes {
        // Lo que ocupa: el texto, el nombre de quien habla y la separación.
        let costo = parte.texto.chars().count() + 12;
        if largo + costo > VENTANA && actual.iter().any(|p| !p.repetida) {
            if util(&actual) {
                fragmentos.extend(armar(&actual, mensajes, usuario));
            }
            let ultima = actual.pop();
            actual.clear();
            largo = 0;
            if let Some(ultima) = ultima {
                let repetida = Parte {
                    mensaje: ultima.mensaje,
                    sigue: ultima.sigue || ultima.texto.chars().count() > SOLAPE,
                    texto: cola(&ultima.texto),
                    repetida: true,
                };
                largo = repetida.texto.chars().count() + 12;
                actual.push(repetida);
            }
        }
        largo += costo;
        actual.push(parte);
    }
    if actual.iter().any(|p| !p.repetida) && util(&actual) {
        fragmentos.extend(armar(&actual, mensajes, usuario));
    }
    fragmentos
}

async fn senal(e: &Estado, conversacion: &str, archivo: &Path, motivo: String) {
    tracing::warn!(conversacion, archivo = %archivo.display(), motivo, "el lector no entendió parte de una conversación");
    let detalle = json!({
        "origen": "lector",
        "motivo": motivo,
        "archivo": archivo.file_name().map(|n| n.to_string_lossy().into_owned()),
    });
    senales::registrar(&e.pool, "error", Some(conversacion), detalle).await;
}

fn posicion(valor: u64) -> i64 {
    i64::try_from(valor).unwrap_or(i64::MAX)
}

/// Borra los pedazos ya guardados de una conversación (su archivo cambió por
/// otro), con sus palabras y sus huellas.
async fn olvidar(e: &Estado, conversacion: &str) -> Resultado<()> {
    let mut tx = base::escribir(&e.pool).await?;
    let ids: Vec<i64> = sqlx::query_scalar("delete from fragmentos where conversacion = ?1 returning id")
        .bind(conversacion)
        .fetch_all(&mut *tx)
        .await?;
    sqlx::query("delete from fragmentos_texto where rowid in (select value from json_each(?1))")
        .bind(base::a_json(&ids))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    for id in ids {
        e.indice.quitar(Clase::Fragmentos, id);
    }
    Ok(())
}

/// Vuelve a armar las palabras de todos los pedazos (cuando faltan, o cuando
/// cambió la forma de sacarles la raíz). De a tandas: pueden ser muchos.
pub async fn reindexar(conn: &mut SqliteConnection) -> Resultado<u64> {
    sqlx::query("delete from fragmentos_texto").execute(&mut *conn).await?;
    let (mut ultimo, mut total) = (0_i64, 0_u64);
    loop {
        let tanda: Vec<(i64, String)> =
            sqlx::query_as("select id, contenido from fragmentos where id > ?1 order by id limit 500")
                .bind(ultimo)
                .fetch_all(&mut *conn)
                .await?;
        let Some((id, _)) = tanda.last() else { break };
        ultimo = *id;
        for (id, contenido) in &tanda {
            sqlx::query("insert into fragmentos_texto (rowid, contenido) values (?1, ?2)")
                .bind(id)
                .bind(texto::para_indexar(contenido))
                .execute(&mut *conn)
                .await?;
        }
        total += tanda.len() as u64;
    }
    Ok(total)
}

/// Lee lo nuevo de una conversación registrada y guarda sus pedazos, con el
/// grupo que tiene ahora. Devuelve cuántos guardó.
pub async fn procesar(e: &Estado, conversacion: &str, archivo: &Path) -> anyhow::Result<usize> {
    let fila: Option<(i64, Option<i64>)> =
        sqlx::query_as("select leida_hasta, grupo_id from conversaciones where id = ?1")
            .bind(conversacion)
            .fetch_optional(&e.pool)
            .await?;
    let (leida_hasta, grupo_id) = fila.with_context(|| format!("no conozco la conversación {conversacion}"))?;
    let tamano =
        tokio::fs::metadata(archivo).await.with_context(|| format!("no pude ver {}", archivo.display()))?.len();
    let mut desde = u64::try_from(leida_hasta).unwrap_or(0);
    if tamano < desde {
        // El archivo es más corto que lo ya leído: es otro. Se empieza de nuevo.
        tracing::warn!(conversacion, "el archivo de la conversación se achicó: lo leo de nuevo entero");
        olvidar(e, conversacion).await?;
        desde = 0;
    }

    let ruta = archivo.to_owned();
    let lectura = tokio::task::spawn_blocking(move || leer(&ruta, desde))
        .await
        .context("se cortó la lectura")?
        .with_context(|| format!("no pude leer {}", archivo.display()))?;
    let fragmentos = fragmentar(&lectura.mensajes, &e.nombre);

    // Un pedazo que ya está en el grupo con el mismo texto no se guarda otra
    // vez: pasa cuando una conversación se abre como copia de otra y su
    // archivo trae repetido todo lo anterior.
    let mut tx = base::escribir(&e.pool).await?;
    let mut vistas: HashSet<String> = HashSet::new();
    let mut guardados = 0;
    for f in &fragmentos {
        let contenido = tapar_secretos(&f.contenido);
        let marca = texto::marca(&contenido);
        if !vistas.insert(marca.clone()) {
            continue;
        }
        let repetido: Option<i64> =
            sqlx::query_scalar("select id from fragmentos where marca = ?1 and grupo_id is ?2 limit 1")
                .bind(&marca)
                .bind(grupo_id)
                .fetch_optional(&mut *tx)
                .await?;
        if repetido.is_some() {
            continue;
        }
        let id: i64 = sqlx::query_scalar(
            "insert into fragmentos (conversacion, grupo_id, desde, hasta, contenido, fecha, marca)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             returning id",
        )
        .bind(conversacion)
        .bind(grupo_id)
        .bind(posicion(f.desde))
        .bind(posicion(f.hasta))
        .bind(contenido.as_ref())
        .bind(f.fecha)
        .bind(&marca)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("insert into fragmentos_texto (rowid, contenido) values (?1, ?2)")
            .bind(id)
            .bind(texto::para_indexar(&contenido))
            .execute(&mut *tx)
            .await?;
        guardados += 1;
    }
    sqlx::query("update conversaciones set leida_hasta = ?2 where id = ?1")
        .bind(conversacion)
        .bind(posicion(lectura.hasta))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    if lectura.raros > 0 {
        senal(e, conversacion, archivo, format!("{} renglones que parecían mensajes y no pude leer", lectura.raros))
            .await;
    } else if lectura.reconocidos == 0 && lectura.hasta - desde >= SOSPECHOSO {
        let motivo = "leí mucho sin reconocer ningún mensaje: puede haber cambiado el formato".to_owned();
        senal(e, conversacion, archivo, motivo).await;
    }
    Ok(guardados)
}

/// Dónde está el archivo de cada conversación ya ubicada, para no buscarlo
/// en cada vuelta.
#[derive(Debug, Default)]
pub struct Ubicaciones(HashMap<String, PathBuf>);

/// Dónde está el archivo de una conversación: `<raíz>/<carpeta>/<id>.jsonl`.
/// Los de subagentes están más adentro y no se miran.
fn ubicar(raiz: &Path, transcripcion: &str, conocidos: &mut Ubicaciones) -> Option<PathBuf> {
    if let Some(conocido) = conocidos.0.get(transcripcion)
        && conocido.is_file()
    {
        return Some(conocido.clone());
    }
    // El id viene de afuera: que no pueda salirse de la carpeta.
    let valido =
        !transcripcion.is_empty() && transcripcion.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valido {
        return None;
    }
    let nombre = format!("{transcripcion}.jsonl");
    for carpeta in std::fs::read_dir(raiz).ok()?.flatten() {
        let candidato = carpeta.path().join(&nombre);
        if candidato.is_file() {
            conocidos.0.insert(transcripcion.to_owned(), candidato.clone());
            return Some(candidato);
        }
    }
    None
}

/// Revisa todas las conversaciones registradas y lee las que tienen algo
/// nuevo y llevan `reposo` sin cambios (o juntaron mucho sin leer). Devuelve
/// cuántos pedazos guardó.
pub async fn recorrer(e: &Estado, raiz: &Path, conocidos: &mut Ubicaciones, reposo: Duration) -> anyhow::Result<usize> {
    let filas: Vec<(String, String, i64)> = sqlx::query_as(
        "select id, transcripcion, leida_hasta from conversaciones
         where transcripcion is not null order by actualizada desc",
    )
    .fetch_all(&e.pool)
    .await?;
    let mut guardados = 0;
    for (id, transcripcion, leida_hasta) in filas {
        let Some(archivo) = ubicar(raiz, &transcripcion, conocidos) else { continue };
        let Ok(datos) = std::fs::metadata(&archivo) else { continue };
        let leido = u64::try_from(leida_hasta).unwrap_or(0);
        if datos.len() == leido {
            continue;
        }
        if datos.len() > leido {
            let quieto = datos.modified().ok().and_then(|m| m.elapsed().ok()).is_none_or(|rato| rato >= reposo);
            if !quieto && datos.len() - leido < MUCHO_PENDIENTE {
                continue;
            }
        }
        match procesar(e, &id, &archivo).await {
            Ok(cuantos) => guardados += cuantos,
            Err(error) => {
                tracing::warn!(conversacion = %id, error = format!("{error:#}"), "no pude leer una conversación");
            }
        }
    }
    if guardados > 0 {
        tracing::info!(guardados, "pedazos de conversaciones guardados");
        e.pendientes.notify_one();
    }
    Ok(guardados)
}

/// La tarea de fondo del lector.
#[must_use]
pub fn tarea(e: Estado, raiz: PathBuf, apagado: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut conocidos = Ubicaciones::default();
        loop {
            if let Err(error) = recorrer(&e, &raiz, &mut conocidos, REPOSO).await {
                tracing::warn!(error = format!("{error:#}"), "falla del lector de conversaciones");
            }
            tokio::select! {
                () = apagado.cancelled() => break,
                () = tokio::time::sleep(CADA) => {},
            }
        }
    })
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn mensaje(quien: Quien, texto: &str, desde: u64) -> Mensaje {
        Mensaje { quien, texto: texto.into(), desde, hasta: desde + 10, fecha: DateTime::UNIX_EPOCH }
    }

    #[test]
    fn saca_lo_que_la_app_mete_en_los_mensajes() {
        let crudo = "<system-reminder>\nNo es del usuario.\n</system-reminder>\nUsemos Cognito para la sesión.\n\n\n\n<pasted_content id=\"1\">un log enorme</pasted_content>\ny avisame.";
        assert_eq!(limpiar(crudo).as_deref(), Some("Usemos Cognito para la sesión.\n\ny avisame."));
        // Lo pegado cierra repitiendo su id.
        let pegado =
            "y este chat? <pasted_content id=\"dd6d\">Vick [4:25 PM] Listo\notro renglón</pasted_content id=\"dd6d\">";
        assert_eq!(limpiar(pegado).as_deref(), Some("y este chat?"));
        assert_eq!(limpiar("<command-name>/model</command-name>\n<command-args>x</command-args>"), None);
        assert_eq!(limpiar("<task-notification><status>completed</status></task-notification>"), None);
        assert_eq!(limpiar("[Request interrupted by user]"), None);
        assert_eq!(limpiar("  "), None);
        let largo = "a".repeat(LARGO_MENSAJE_USUARIO + 50);
        assert_eq!(limpiar(&largo).unwrap().chars().count(), LARGO_MENSAJE_USUARIO + 4);
    }

    fn leido(renglon: &str) -> Option<(Quien, String)> {
        mensaje_de(renglon.as_bytes()).expect("se entiende").map(|d| (d.quien, d.texto))
    }

    #[test]
    fn lee_solo_lo_que_dijeron_el_usuario_y_claude() {
        let del_usuario = r#"{"type":"user","origin":{"kind":"human"},"timestamp":"2026-11-12T18:27:01.745Z","message":{"role":"user","content":"Guardá la decisión"}}"#;
        assert_eq!(leido(del_usuario), Some((Quien::Usuario, "Guardá la decisión".into())));
        let fecha = mensaje_de(del_usuario.as_bytes()).unwrap().unwrap().fecha.unwrap();
        assert_eq!(fecha.to_rfc3339(), "2026-11-12T18:27:01.745+00:00");

        // De Claude solo el texto: ni lo que piensa ni las herramientas.
        let de_claude = r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"thinking","thinking":"mmm"},{"type":"text","text":"Listo, guardada."},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert_eq!(leido(de_claude), Some((Quien::Claude, "Listo, guardada.".into())));

        // Reconocidos, pero sin texto que guardar.
        let sin_texto = [
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"salida larga"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"aviso de la app"}}"#,
            r#"{"type":"user","isCompactSummary":true,"message":{"content":"resumen"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"de un subagente"}}"#,
            r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"content":"terminó la tarea"}}"#,
            r#"{"type":"user","origin":"peer","message":{"content":"de otra conversación"}}"#,
            r#"{"type":"assistant","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"API Error"}]}}"#,
            r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{}}]}}"#,
        ];
        for renglon in sin_texto {
            assert_eq!(leido(renglon).map(|(_, t)| t).as_deref(), Some(""), "{renglon}");
        }

        // Otros renglones del archivo ni se miran.
        assert_eq!(leido(r#"{"type":"queue-operation","content":"x"}"#), None);
        assert_eq!(leido(r#"{"type":"api-request-blob","message":{"role":"user","content":[{"type":"user"}]}}"#), None);
        // Mensajes viejos, sin el dato de origen, valen.
        assert_eq!(
            leido(r#"{"type":"user","message":{"content":[{"type":"text","text":"hola"}]}}"#),
            Some((Quien::Usuario, "hola".into()))
        );
        // Parecía un mensaje y no se entiende: se avisa.
        assert!(mensaje_de(br#"{"type":"user","message":{"content":"#).is_err());
        assert!(mensaje_de(br#"{"type":"user","message":{"content":42}}"#).is_err());
    }

    #[test]
    fn corta_los_textos_largos_en_los_espacios() {
        assert_eq!(cortar("uno dos tres cuatro", 8), vec!["uno dos", "tres", "cuatro"]);
        assert_eq!(cortar("corto", 8), vec!["corto"]);
        assert_eq!(cortar("sinespaciosmuylargo", 8), vec!["sinespac", "iosmuyla", "rgo"]);
        assert_eq!(cortar("ñandú ñandú", 5), vec!["ñandú", "ñandú"]);
    }

    #[test]
    fn un_intercambio_corto_es_un_solo_pedazo() {
        let mensajes = [
            mensaje(Quien::Usuario, "¿Con qué mandamos los mails del Inbox?", 100),
            mensaje(Quien::Claude, "Con SendGrid, la misma cuenta de Tienda.", 200),
            mensaje(Quien::Claude, "La API key está en Secrets Manager.", 300),
        ];
        let pedazos = fragmentar(&mensajes, "Usuario");
        assert_eq!(pedazos.len(), 1);
        assert_eq!(
            pedazos[0].contenido,
            "Usuario: ¿Con qué mandamos los mails del Inbox?\n\nClaude: Con SendGrid, la misma cuenta de Tienda.\n\nLa API key está en Secrets Manager."
        );
        assert_eq!((pedazos[0].desde, pedazos[0].hasta), (100, 310));
    }

    #[test]
    fn una_conversacion_larga_se_parte_con_algo_repetido() {
        let parrafo = |n: usize| format!("Párrafo {n}: {}", "palabra ".repeat(60).trim_end());
        let largo = (1..=8).map(parrafo).collect::<Vec<_>>().join("\n\n");
        let mensajes =
            [mensaje(Quien::Usuario, "Contame todo el plan de la migración.", 0), mensaje(Quien::Claude, &largo, 50)];
        let pedazos = fragmentar(&mensajes, "Usuario");
        assert!(pedazos.len() >= 3, "{}", pedazos.len());
        for p in &pedazos {
            assert!(p.contenido.chars().count() <= VENTANA + 20, "{}", p.contenido.chars().count());
        }
        assert!(pedazos[0].contenido.starts_with("Usuario: Contame todo el plan"));
        // El segundo retoma la respuesta: dice quién habla y repite el final del anterior.
        assert!(pedazos[1].contenido.starts_with("Claude (sigue): …"), "{}", &pedazos[1].contenido[..60]);
        let repetido = pedazos[1].contenido.lines().next().unwrap().trim_start_matches("Claude (sigue): …").trim();
        assert!(pedazos[0].contenido.ends_with(repetido), "«{repetido}»");
        // Todos los párrafos quedaron en algún pedazo.
        for n in 1..=8 {
            assert!(pedazos.iter().any(|p| p.contenido.contains(&format!("Párrafo {n}:"))), "falta el {n}");
        }
        assert_eq!((pedazos[1].desde, pedazos[1].hasta), (50, 60));
    }

    #[test]
    fn lo_muy_corto_no_se_guarda() {
        assert_eq!(fragmentar(&[mensaje(Quien::Usuario, "sí, dale", 0)], "Usuario"), Vec::<Fragmento>::new());
        assert_eq!(fragmentar(&[], "Usuario"), Vec::<Fragmento>::new());
    }
}
