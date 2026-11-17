//! Slack como fuente (punto 4.11 del diseño): lo que el usuario escribió en
//! sus espacios de trabajo, pedido a Slack con una llave de solo lectura.
//!
//! A cada espacio se le pide que busque los mensajes del propio usuario, del
//! más viejo al más nuevo, y queda anotado hasta dónde se llegó: la vez
//! siguiente se pide solo lo de los últimos días. Entra solo lo que escribió
//! él: lo de los demás no se pide ni se guarda.
//!
//! La llave de cada espacio vive en el llavero de macOS. La memoria la pide
//! cada vez que lee, la usa para hablar con Slack y la suelta: no la copia a
//! ningún archivo ni la anota en la base o en los registros.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Utc};
use regex::{Captures, Regex};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sqlx::SqlitePool;

use super::fuentes::excluido;
use super::propios::{self, Propio};
use crate::config::{API_DE_SLACK, Espacio, Llave, Slack};

pub const FUENTE: &str = "slack";
/// Lo que se le pide a Slack: quién es el dueño de la llave, sus mensajes y
/// el nombre de una persona.
const QUIEN_SOY: &str = "auth.test";
const BUSCAR: &str = "search.messages";
const PERSONA: &str = "users.info";
/// Cuántos mensajes se piden por vez: es el tope de Slack.
const POR_PAGINA: &str = "100";
/// Cuántas páginas se leen como mucho en una pasada.
const PAGINAS_MAXIMAS: usize = 2_000;
/// Cuántos días para atrás se vuelve a pedir en cada lectura: Slack filtra
/// por día, y tarda un rato en dejar buscar lo recién escrito.
const DIAS_DE_REPASO: i64 = 2;
/// Cuántas veces se insiste si Slack pide ir más despacio, y cuánto se
/// espera como mucho cada vez.
const REINTENTOS: usize = 5;
const ESPERA_MAXIMA: Duration = Duration::from_secs(120);
/// Lo que se espera si Slack pide ir más despacio y no dice cuánto.
const ESPERA_SIN_DECIR: u64 = 5;
const LIMITE_DEL_PEDIDO: Duration = Duration::from_secs(30);
/// El llavero puede quedar esperando a que el usuario conteste un cartel.
const LIMITE_DEL_LLAVERO: Duration = Duration::from_secs(120);
/// Con qué cuenta están guardadas las llaves en el llavero.
const CUENTA_DEL_LLAVERO: &str = "token";
/// El ayudante de Slack: lo que se le escribe no es para nadie.
const SLACKBOT: &str = "USLACKBOT";
const MICROS: i64 = 1_000_000;

/// Lo que salió de leer Slack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Juntado {
    /// Mensajes del usuario que devolvió Slack y no estaban leídos (si solo
    /// se estaba probando: todos los que tiene).
    pub leidos: usize,
    /// Los que quedaron como suyos después de filtrar.
    pub propios: usize,
    /// Los que no estaban guardados.
    pub nuevos: usize,
    /// En cuántas conversaciones.
    pub chats: usize,
}

/// Lo que pasó con todos los espacios de trabajo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lectura {
    /// La suma de los espacios que se pudieron leer.
    pub juntado: Juntado,
    /// Cuántos espacios se pudieron leer.
    pub espacios: usize,
    /// De cada espacio que no se pudo leer, por qué, dicho para el usuario.
    pub fallas: Vec<String>,
}

/// Una llave en memoria: no se puede mostrar ni por descuido.
struct Secreto(String);

impl std::fmt::Debug for Secreto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secreto(…)")
    }
}

/// Le pide al llavero de macOS la llave de un espacio. Si el llavero le
/// muestra un cartel al usuario, espera un rato a que conteste.
async fn llave_de(espacio: &Espacio) -> anyhow::Result<Secreto> {
    let entrada = match &espacio.llave {
        Llave::Fija(llave) => return Ok(Secreto(llave.clone())),
        Llave::Llavero(entrada) => entrada,
    };
    let mut orden = tokio::process::Command::new("/usr/bin/security");
    orden
        .args(["find-generic-password", "-s", entrada, "-a", CUENTA_DEL_LLAVERO, "-w"])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let salida = tokio::time::timeout(LIMITE_DEL_LLAVERO, orden.output())
        .await
        .map_err(|_| anyhow!("el llavero no contestó: puede haber un cartel esperando una respuesta en la Mac"))?
        .context("no pude pedirle la llave al llavero de macOS")?;
    let llave = String::from_utf8_lossy(&salida.stdout).trim().to_owned();
    if !salida.status.success() || llave.is_empty() {
        bail!(
            "no encontré la llave en el llavero (entrada «{entrada}», cuenta «{CUENTA_DEL_LLAVERO}»). Se guarda \
             con: security add-generic-password -U -s {entrada} -a {CUENTA_DEL_LLAVERO} -w"
        );
    }
    Ok(Secreto(llave))
}

/// Por qué Slack no quiso contestar, dicho para el usuario.
fn falla_de_slack(metodo: &str, error: &str, falta: Option<&str>) -> anyhow::Error {
    match error {
        "invalid_auth" | "not_authed" | "token_revoked" | "token_expired" | "account_inactive" => {
            anyhow!("la llave no sirve o fue anulada: hay que crear otra en Slack y guardarla en el llavero")
        }
        "missing_scope" => anyhow!(
            "a la llave le falta un permiso de Slack ({}): hay que agregárselo a la app y volver a instalarla en el \
             espacio",
            falta.unwrap_or("no dice cuál")
        ),
        otro => anyhow!("Slack contestó «{otro}» al pedirle {metodo}"),
    }
}

/// Quien habla con Slack en nombre de un espacio.
struct Cliente {
    http: reqwest::Client,
    api: String,
    llave: Secreto,
    pausa: Duration,
    /// Cuándo salió la última búsqueda.
    ultima_busqueda: Option<Instant>,
}

impl Cliente {
    fn nuevo(cfg: &Slack, llave: Secreto) -> anyhow::Result<Self> {
        let api = cfg.api.trim_end_matches('/').to_owned();
        let de_verdad = api == API_DE_SLACK;
        let de_prueba = ["http://127.0.0.1:", "http://localhost:"].iter().any(|local| api.starts_with(local));
        if !de_verdad && !de_prueba {
            bail!("la llave de Slack no se le manda a nadie que no sea Slack");
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(LIMITE_DEL_PEDIDO)
            .https_only(de_verdad)
            // Slack no manda a otro lado: si alguien lo hace, la llave no lo sigue.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("no pude preparar la conexión con Slack")?;
        Ok(Self { http, api, llave, pausa: cfg.pausa, ultima_busqueda: None })
    }

    /// Espera lo que falte para no buscar más seguido de lo que deja Slack.
    async fn esperar_turno(&mut self) {
        if let Some(falta) = self.ultima_busqueda.and_then(|ultima| self.pausa.checked_sub(ultima.elapsed())) {
            tokio::time::sleep(falta).await;
        }
        self.ultima_busqueda = Some(Instant::now());
    }

    /// Le pide algo a Slack. Si contesta que vaya más despacio, espera lo
    /// que diga y vuelve a pedir.
    async fn pedir<T: DeserializeOwned>(&mut self, metodo: &str, datos: &[(&str, &str)]) -> anyhow::Result<T> {
        for _ in 0..=REINTENTOS {
            if metodo == BUSCAR {
                self.esperar_turno().await;
            }
            let respuesta = self
                .http
                .get(format!("{}/{metodo}", self.api))
                .bearer_auth(&self.llave.0)
                .query(datos)
                .send()
                .await
                .map_err(|falla| anyhow::Error::from(falla.without_url()).context("no pude hablar con Slack"))?;
            let pide_esperar =
                |segundos: Option<u64>| Duration::from_secs(segundos.unwrap_or(ESPERA_SIN_DECIR)).min(ESPERA_MAXIMA);
            if respuesta.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let segundos = respuesta
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|valor| valor.to_str().ok())
                    .and_then(|valor| valor.trim().parse::<u64>().ok());
                tokio::time::sleep(pide_esperar(segundos)).await;
                continue;
            }
            if !respuesta.status().is_success() {
                bail!("Slack contestó {} al pedirle {metodo}", respuesta.status());
            }
            let cuerpo: serde_json::Value = respuesta.json().await.map_err(|falla| {
                anyhow::Error::from(falla.without_url()).context("Slack contestó algo que no se puede leer")
            })?;
            if cuerpo["ok"].as_bool() != Some(true) {
                let error = cuerpo["error"].as_str().unwrap_or("un error sin nombre");
                if error == "ratelimited" {
                    tokio::time::sleep(pide_esperar(None)).await;
                    continue;
                }
                return Err(falla_de_slack(metodo, error, cuerpo["needed"].as_str()));
            }
            return serde_json::from_value(cuerpo)
                .with_context(|| format!("Slack contestó a {metodo} algo con otra forma que la esperada"));
        }
        bail!("Slack pidió ir más despacio demasiadas veces seguidas")
    }
}

/// El dueño de la llave.
#[derive(Debug, Deserialize)]
struct Quien {
    user_id: String,
    /// Su nombre de usuario: así figura en los nombres de los grupos chicos.
    #[serde(default)]
    user: String,
}

#[derive(Debug, Deserialize)]
struct Busqueda {
    messages: Encontrados,
    #[serde(default)]
    response_metadata: Option<Paginado>,
}

#[derive(Debug, Default, Deserialize)]
struct Encontrados {
    #[serde(default)]
    total: usize,
    #[serde(default)]
    matches: Vec<Encontrado>,
    #[serde(default)]
    pagination: Option<Paginado>,
}

#[derive(Debug, Default, Deserialize)]
struct Paginado {
    #[serde(default)]
    next_cursor: Option<String>,
}

impl Busqueda {
    /// Con qué se pide la página que sigue, si hay.
    fn sigue(&self) -> Option<&str> {
        [self.messages.pagination.as_ref(), self.response_metadata.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|paginado| paginado.next_cursor.as_deref())
            .find(|cursor| !cursor.is_empty())
    }
}

#[derive(Debug, Deserialize)]
struct Encontrado {
    #[serde(default)]
    ts: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    channel: Option<Canal>,
}

#[derive(Debug, Deserialize)]
struct Canal {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    is_im: bool,
    #[serde(default)]
    is_mpim: bool,
    /// En un mensaje directo, la otra persona.
    #[serde(default)]
    user: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DatosDePersona {
    user: Persona,
}

#[derive(Debug, Default, Deserialize)]
struct Persona {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    real_name: Option<String>,
    #[serde(default)]
    profile: Option<Perfil>,
}

#[derive(Debug, Default, Deserialize)]
struct Perfil {
    #[serde(default)]
    real_name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

impl Persona {
    /// Cómo se llama: su nombre, o si no cómo se muestra, o su usuario.
    fn nombre(self) -> Option<String> {
        let perfil = self.profile.unwrap_or_default();
        [self.real_name, perfil.real_name, perfil.display_name, self.name]
            .into_iter()
            .flatten()
            .map(|nombre| nombre.trim().to_owned())
            .find(|nombre| !nombre.is_empty())
    }
}

/// El momento de un mensaje, que Slack da en segundos con seis decimales
/// («1763310000.000200»), en millonésimas de segundo.
fn micros_de(ts: &str) -> Option<i64> {
    let (segundos, resto) = ts.trim().split_once('.').unwrap_or((ts.trim(), ""));
    if segundos.is_empty() || !segundos.chars().chain(resto.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let decimales: String = resto.chars().chain(std::iter::repeat('0')).take(6).collect();
    segundos.parse::<i64>().ok()?.checked_mul(MICROS)?.checked_add(decimales.parse::<i64>().ok()?)
}

/// El día (para Slack) desde el que hay que pedir para no perder nada de lo
/// escrito después de `hasta`.
fn repaso_desde(hasta: i64) -> Option<String> {
    let momento = DateTime::<Utc>::from_timestamp_micros(hasta)? - chrono::Duration::days(DIAS_DE_REPASO);
    Some(momento.format("%Y-%m-%d").to_string())
}

/// Un pedazo de código entre tres acentos graves: no lo escribió a mano.
static CODIGO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```.*?```").expect("patrón válido"));
/// Lo que Slack pone entre «<» y «>»: una mención, un canal, un enlace.
static MARCA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([^<>\n]+)>").expect("patrón válido"));
/// El tono de piel que sigue a un emoji.
static TONO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":skin-tone-[2-6]:").expect("patrón válido"));
/// Un emoji escrito con su nombre: «:pray:».
static EMOJI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":([a-z0-9_+\-]+):").expect("patrón válido"));

/// Los emojis que más se usan, con el nombre que les da Slack. Los demás
/// quedan con su nombre.
const EMOJIS: &[(&str, &str)] = &[
    ("+1", "👍"),
    ("thumbsup", "👍"),
    ("-1", "👎"),
    ("pray", "🙏"),
    ("joy", "😂"),
    ("rolling_on_the_floor_laughing", "🤣"),
    ("smile", "😄"),
    ("grin", "😁"),
    ("grinning", "😀"),
    ("slightly_smiling_face", "🙂"),
    ("blush", "😊"),
    ("wink", "😉"),
    ("sweat_smile", "😅"),
    ("laughing", "😆"),
    ("thinking_face", "🤔"),
    ("sob", "😭"),
    ("cry", "😢"),
    ("disappointed", "😞"),
    ("heart", "❤️"),
    ("fire", "🔥"),
    ("rocket", "🚀"),
    ("tada", "🎉"),
    ("eyes", "👀"),
    ("white_check_mark", "✅"),
    ("x", "❌"),
    ("warning", "⚠️"),
    ("muscle", "💪"),
    ("ok_hand", "👌"),
    ("wave", "👋"),
    ("raised_hands", "🙌"),
    ("clap", "👏"),
    ("100", "💯"),
    ("point_up", "☝️"),
    ("point_down", "👇"),
    ("point_right", "👉"),
    ("facepalm", "🤦"),
    ("shrug", "🤷"),
    ("sunglasses", "😎"),
    ("upside_down_face", "🙃"),
    ("handshake", "🤝"),
    ("scream", "😱"),
    ("partying_face", "🥳"),
];

/// Lo que queda de una marca de Slack al pasarla a texto.
fn marca_a_texto(marca: &Captures<'_>) -> String {
    let adentro = &marca[1];
    let (destino, etiqueta) = adentro.split_once('|').map_or((adentro, None), |(d, e)| (d, Some(e.trim())));
    let etiqueta = etiqueta.filter(|e| !e.is_empty());
    match destino.chars().next() {
        // A quién nombró no hace a cómo escribe, y el nombre no viene.
        Some('@') => "@alguien".to_owned(),
        Some('#') => format!("#{}", etiqueta.unwrap_or("canal")),
        Some('!') => match destino {
            "!here" | "!channel" | "!everyone" => format!("@{}", &destino[1..]),
            _ => etiqueta.unwrap_or_default().to_owned(),
        },
        _ => etiqueta.unwrap_or_else(|| destino.strip_prefix("mailto:").unwrap_or(destino)).to_owned(),
    }
}

/// Un mensaje como lo guarda Slack, pasado a como lo escribió el usuario: sin
/// el código pegado ni lo que citó de otro, con las menciones, los canales y
/// los enlaces como texto, y con los emojis más comunes.
fn texto_llano(texto: &str) -> String {
    let sin_codigo = CODIGO.replace_all(texto, "\n");
    let mut propios: Vec<&str> = Vec::new();
    for renglon in sin_codigo.lines() {
        let principio = renglon.trim_start();
        // «>>>» cita todo lo que sigue; «>», ese renglón.
        if principio.starts_with("&gt;&gt;&gt;") {
            break;
        }
        if !principio.starts_with("&gt;") {
            propios.push(renglon);
        }
    }
    let sin_marcas = MARCA.replace_all(&propios.join("\n"), marca_a_texto).into_owned();
    let legible = sin_marcas.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&");
    let sin_tonos = TONO.replace_all(&legible, "");
    EMOJI
        .replace_all(&sin_tonos, |emoji: &Captures<'_>| {
            let dibujo = EMOJIS.iter().find(|(nombre, _)| *nombre == &emoji[1]).map(|(_, dibujo)| *dibujo);
            dibujo.map_or_else(|| emoji[0].to_owned(), str::to_owned)
        })
        .trim()
        .to_owned()
}

/// Los demás de un grupo chico, sacados de su nombre interno
/// («mpdm-ana--luis--pedro-1»).
fn del_grupo_chico(nombre: &str, yo: &str) -> String {
    let medio = nombre.strip_prefix("mpdm-").unwrap_or(nombre);
    let medio = match medio.rsplit_once('-') {
        Some((antes, numero)) if !numero.is_empty() && numero.chars().all(|c| c.is_ascii_digit()) => antes,
        _ => medio,
    };
    let otros: Vec<&str> = medio.split("--").filter(|parte| !parte.is_empty() && *parte != yo).collect();
    if otros.is_empty() { nombre.to_owned() } else { otros.join(", ") }
}

/// Los mensajes de una conversación que quedaron como propios.
#[derive(Default)]
struct DelChat {
    para: Option<String>,
    mensajes: Vec<Propio>,
}

/// Quien lee un espacio de trabajo.
struct Lector<'a> {
    cliente: Cliente,
    espacio: &'a str,
    yo: Quien,
    sin: &'a [String],
    /// El nombre de cada persona por la que ya se preguntó.
    nombres: HashMap<String, Option<String>>,
}

impl Lector<'_> {
    /// Cómo se llama una persona. Si Slack no lo dice, queda sin nombre.
    async fn nombre_de(&mut self, persona: &str) -> Option<String> {
        if let Some(sabido) = self.nombres.get(persona) {
            return sabido.clone();
        }
        let nombre = match self.cliente.pedir::<DatosDePersona>(PERSONA, &[("user", persona)]).await {
            Ok(datos) => datos.user.nombre(),
            Err(falla) => {
                tracing::debug!(
                    espacio = self.espacio,
                    falla = format!("{falla:#}"),
                    "Slack no dio el nombre de alguien"
                );
                None
            }
        };
        self.nombres.insert(persona.to_owned(), nombre.clone());
        nombre
    }

    /// A quién le escribía en una conversación: la persona, los del grupo
    /// chico o el canal. `None` si la conversación no se lee: las notas que
    /// se deja a sí mismo y lo que le escribe al ayudante de Slack.
    async fn para(&mut self, canal: &Canal) -> Option<Option<String>> {
        if canal.is_im {
            let otro = canal.user.as_deref().unwrap_or(&canal.name);
            if otro == self.yo.user_id || otro == SLACKBOT {
                return None;
            }
            return Some(self.nombre_de(otro).await);
        }
        if canal.is_mpim {
            return Some(Some(del_grupo_chico(&canal.name, &self.yo.user)));
        }
        // El mismo canal puede existir en dos espacios: cada uno es otro.
        Some(Some(format!("#{} ({})", canal.name, self.espacio)))
    }

    /// De una página de resultados, lo que es del usuario y no estaba leído,
    /// por conversación; y el momento del mensaje más nuevo que se vio.
    async fn propios_de(
        &mut self,
        pagina: &[Encontrado],
        desde: i64,
        nombre: &str,
        juntado: &mut Juntado,
    ) -> (HashMap<String, DelChat>, i64) {
        let mut por_chat: HashMap<String, DelChat> = HashMap::new();
        let mut mayor = desde;
        for mensaje in pagina {
            let (Some(micros), Some(canal)) = (micros_de(&mensaje.ts), &mensaje.channel) else { continue };
            // Lo que no es suyo no entra, aunque Slack lo devuelva.
            if micros <= desde || mensaje.user.as_deref().is_some_and(|autor| autor != self.yo.user_id) {
                continue;
            }
            mayor = mayor.max(micros);
            let Some(para) = self.para(canal).await else { continue };
            if excluido(para.as_deref(), self.sin) {
                continue;
            }
            juntado.leidos += 1;
            let (Some(texto), Some(fecha)) = (
                propios::lo_propio(&texto_llano(&mensaje.text), nombre),
                DateTime::<Utc>::from_timestamp_micros(micros),
            ) else {
                continue;
            };
            let del_chat = por_chat.entry(format!("slack:{}:{}", self.espacio, canal.id)).or_default();
            del_chat.para = del_chat.para.take().or(para);
            del_chat.mensajes.push(Propio { texto, fecha });
            juntado.propios += 1;
        }
        (por_chat, mayor)
    }
}

/// Lee un espacio de trabajo desde donde se había llegado. Guarda página por
/// página: si se corta, la vez siguiente sigue desde ahí.
async fn juntar_espacio(
    pool: &SqlitePool,
    cfg: &Slack,
    espacio: &Espacio,
    nombre: &str,
    anotar: bool,
) -> anyhow::Result<Juntado> {
    let mut cliente = Cliente::nuevo(cfg, llave_de(espacio).await?)?;
    let yo: Quien = cliente.pedir(QUIEN_SOY, &[]).await?;
    let mios = format!("from:<@{}>", yo.user_id);
    let mut juntado = Juntado::default();
    if !anotar {
        // Para ver qué entraría alcanza con preguntar cuántos hay.
        let datos = [("query", mios.as_str()), ("count", "1")];
        juntado.leidos = cliente.pedir::<Busqueda>(BUSCAR, &datos).await?.messages.total;
        return Ok(juntado);
    }
    let leido: Option<i64> = sqlx::query_scalar("select hasta from lecturas_propias where fuente = ?1 and origen = ?2")
        .bind(FUENTE)
        .bind(&espacio.nombre)
        .fetch_optional(pool)
        .await?;
    let desde = leido.unwrap_or(0);
    let consulta = match repaso_desde(desde).filter(|_| desde > 0) {
        Some(dia) => format!("{mios} after:{dia}"),
        None => mios,
    };
    let mut lector = Lector { cliente, espacio: &espacio.nombre, yo, sin: &cfg.sin, nombres: HashMap::new() };
    let mut chats: HashSet<String> = HashSet::new();
    let mut cursor = "*".to_owned();
    let mut hasta = desde;
    for _ in 0..PAGINAS_MAXIMAS {
        let datos = [
            ("query", consulta.as_str()),
            ("count", POR_PAGINA),
            ("sort", "timestamp"),
            ("sort_dir", "asc"),
            ("cursor", cursor.as_str()),
        ];
        let pagina: Busqueda = lector.cliente.pedir(BUSCAR, &datos).await?;
        let (por_chat, mayor) = lector.propios_de(&pagina.messages.matches, desde, nombre, &mut juntado).await;
        for (clave, del_chat) in &por_chat {
            juntado.nuevos +=
                super::guardar(pool, FUENTE, FUENTE, Some(clave), del_chat.para.as_deref(), &del_chat.mensajes).await?;
        }
        chats.extend(por_chat.into_keys());
        if mayor > hasta {
            hasta = mayor;
            sqlx::query(
                "insert into lecturas_propias (fuente, origen, hasta, tamano) values (?1, ?2, ?3, 0)
                 on conflict (fuente, origen) do update set hasta = excluded.hasta",
            )
            .bind(FUENTE)
            .bind(&espacio.nombre)
            .bind(hasta)
            .execute(pool)
            .await?;
        }
        match pagina.sigue() {
            Some(sigue) if !pagina.messages.matches.is_empty() => cursor = sigue.to_owned(),
            _ => break,
        }
    }
    juntado.chats = chats.len();
    Ok(juntado)
}

/// Lee lo que el usuario (`nombre`) escribió en cada espacio de Slack desde
/// la última vez y, con `anotar`, lo guarda en `pool`. Sin `anotar` solo
/// pregunta cuántos mensajes suyos hay. Un espacio que no se puede leer no
/// frena a los demás.
pub async fn juntar(pool: &SqlitePool, cfg: &Slack, nombre: &str, anotar: bool) -> Lectura {
    let mut lectura = Lectura::default();
    for espacio in &cfg.espacios {
        match juntar_espacio(pool, cfg, espacio, nombre, anotar).await {
            Ok(juntado) => {
                lectura.espacios += 1;
                lectura.juntado.leidos += juntado.leidos;
                lectura.juntado.propios += juntado.propios;
                lectura.juntado.nuevos += juntado.nuevos;
                lectura.juntado.chats += juntado.chats;
            }
            Err(falla) => lectura.fallas.push(format!("«{}»: {falla:#}", espacio.nombre)),
        }
    }
    lectura
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn pasa_lo_de_slack_a_como_lo_escribio() {
        assert_eq!(texto_llano("hola <@U0ANA1>, lo viste?"), "hola @alguien, lo viste?");
        assert_eq!(
            texto_llano("lo dejé en <#C0EQUIPO|equipo-acme> y en <#C0OTRO>"),
            "lo dejé en #equipo-acme y en #canal"
        );
        assert_eq!(texto_llano("<!here> arrancamos en 5"), "@here arrancamos en 5");
        assert_eq!(texto_llano("aviso a <!subteam^S0GRUPO|@soporte> ahora"), "aviso a @soporte ahora");
        assert_eq!(
            texto_llano("mirá <https://example.com/tablero|el tablero> o <https://example.com/otro>"),
            "mirá el tablero o https://example.com/otro"
        );
        assert_eq!(texto_llano("escribile a <mailto:ana@example.com|ana@example.com>"), "escribile a ana@example.com");
        assert_eq!(texto_llano("si a &lt; b &amp;&amp; b &gt; c"), "si a < b && b > c");
        // Lo que escribió con signos de menor y mayor no es una marca.
        assert_eq!(texto_llano("poné &lt;tu nombre&gt; ahí"), "poné <tu nombre> ahí");
    }

    #[test]
    fn deja_afuera_lo_citado_y_el_codigo_pegado() {
        assert_eq!(texto_llano("&gt; ¿llegás a la reunión?\nsí, llego justo"), "sí, llego justo");
        assert_eq!(texto_llano("te contesto abajo\n&gt;&gt;&gt; un texto largo\nde otra persona"), "te contesto abajo");
        assert_eq!(texto_llano("probá con esto\n```\ncargo test\n```\ny avisame"), "probá con esto\n\n\ny avisame");
        // Un pedazo de código sin cerrar queda, y después no pasa por escrito a mano.
        assert!(texto_llano("mirá ```let a = 1;").contains("```"));
    }

    #[test]
    fn reconoce_los_emojis_mas_comunes() {
        assert_eq!(texto_llano("gracias :pray:"), "gracias 🙏");
        assert_eq!(texto_llano("dale :+1::skin-tone-3: mañana lo veo"), "dale 👍 mañana lo veo");
        // Uno que no conoce queda con su nombre, y una hora no es un emoji.
        assert_eq!(texto_llano("listo :acme-party: a las 10:30:45"), "listo :acme-party: a las 10:30:45");
    }

    #[test]
    fn lee_el_momento_de_cada_mensaje() {
        assert_eq!(micros_de("1763310000.000200"), Some(1_763_310_000_000_200));
        assert_eq!(micros_de("1763310000.5"), Some(1_763_310_000_500_000));
        assert_eq!(micros_de("1763310000"), Some(1_763_310_000_000_000));
        for malo in ["", "ayer", "12.3x", "-5.000000", "99999999999999999999.0"] {
            assert_eq!(micros_de(malo), None, "{malo}");
        }
        // 2026-11-16T16:20:00Z: se vuelve a pedir desde dos días antes.
        assert_eq!(repaso_desde(1_794_846_000_000_200).as_deref(), Some("2026-11-14"));
    }

    #[test]
    fn nombra_a_los_demas_de_un_grupo_chico() {
        assert_eq!(del_grupo_chico("mpdm-ana--luis.gomez--pedro-1", "ana"), "luis.gomez, pedro");
        assert_eq!(del_grupo_chico("mpdm-luis--ana-12", "ana"), "luis");
        assert_eq!(del_grupo_chico("un-nombre-raro", "ana"), "un-nombre-raro");
    }

    #[test]
    fn el_nombre_de_una_persona_sale_de_donde_haya() {
        let con = |real: Option<&str>, mostrado: Option<&str>, usuario: Option<&str>| Persona {
            name: usuario.map(Into::into),
            real_name: None,
            profile: Some(Perfil { real_name: real.map(Into::into), display_name: mostrado.map(Into::into) }),
        };
        assert_eq!(con(Some(" Luis Gómez "), Some("luisg"), Some("lgomez")).nombre().as_deref(), Some("Luis Gómez"));
        assert_eq!(con(Some("  "), Some("luisg"), Some("lgomez")).nombre().as_deref(), Some("luisg"));
        assert_eq!(con(None, None, Some("lgomez")).nombre().as_deref(), Some("lgomez"));
        assert_eq!(Persona::default().nombre(), None);
    }

    #[test]
    fn la_llave_solo_se_le_manda_a_slack() {
        let cfg = |api: &str| Slack { espacios: vec![], sin: vec![], api: api.to_owned(), pausa: Duration::ZERO };
        let llave = || Secreto("xoxp-de-mentira".to_owned());
        assert!(Cliente::nuevo(&cfg(API_DE_SLACK), llave()).is_ok());
        assert!(Cliente::nuevo(&cfg("http://127.0.0.1:4000"), llave()).is_ok());
        for otro in ["https://example.com/api", "http://slack.com/api", "https://slack.com.example.com/api"] {
            assert!(Cliente::nuevo(&cfg(otro), llave()).is_err(), "{otro}");
        }
        assert_eq!(format!("{:?}", llave()), "Secreto(…)");
    }

    #[test]
    fn dice_que_hacer_cuando_slack_no_acepta_la_llave() {
        let anulada = falla_de_slack(BUSCAR, "token_revoked", None).to_string();
        assert!(anulada.contains("no sirve o fue anulada"), "{anulada}");
        let sin_permiso = falla_de_slack(BUSCAR, "missing_scope", Some("search:read")).to_string();
        assert!(sin_permiso.contains("le falta un permiso de Slack (search:read)"), "{sin_permiso}");
        assert_eq!(
            falla_de_slack(BUSCAR, "fatal_error", None).to_string(),
            "Slack contestó «fatal_error» al pedirle search.messages"
        );
    }
}
