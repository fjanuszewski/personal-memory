//! Slack leído por la conexión de Claude (punto 4.11 del diseño): para un
//! espacio de trabajo del que no hay llave en esta máquina, pero que el
//! usuario ya tiene conectado a Claude.
//!
//! El servicio no puede usar esa conexión: vive dentro de Claude. La usa el
//! mod de una conversación abierta, que hace de mensajero: le pregunta al
//! servicio qué pedir, se lo pide a Slack por la conexión y le entrega lo que
//! contestó, tal cual. Todo lo demás se decide acá: cuándo toca, quién lee
//! (una conversación por vez), qué es del usuario y hasta dónde se llegó.
//!
//! Lo que contesta la conexión es un texto pensado para que lo lea Claude, no
//! un formato que alguien prometa mantener. Si cambia, la lectura falla y
//! queda avisado: no se guarda nada a medio entender.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::fuentes::{self, Juntada, excluido};
use super::propios::{self, Propio};
use super::slack::{self, Juntado, SLACKBOT};
use crate::base;
use crate::config::{Llave, Slack};
use crate::error::Resultado;
use crate::estado::Estado;

/// Cómo nombra el mod a la conexión de Slack de Claude.
const CONEXION_DE_SLACK: &str = "slack";
/// Cuánto dura el turno de una conversación sin que entregue nada: después
/// de eso, otra puede seguir desde donde quedó.
const DURA_EL_TURNO: Duration = Duration::from_secs(180);
/// Cuántas páginas se leen como mucho en un turno.
const PAGINAS_POR_TURNO: usize = 2_000;
/// Cuánto espera el mod entre un pedido a la conexión y el siguiente.
const PAUSA_MS: u64 = 1_500;
/// Lo más largo que se anota de por qué el mod no pudo pedir.
const LARGO_DEL_MOTIVO: usize = 300;
const MICROS: i64 = 1_000_000;

/// Lo que el mod trae de una fuente que se lee por una conexión de Claude, o
/// con qué conexiones cuenta para que se le pida algo. No es para Claude.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct PasoDeFuente {
    /// Las conexiones de Claude que tiene esa conversación. Hoy importa una: `slack`.
    #[serde(default)]
    pub conexiones: Vec<String>,
    /// El espacio de trabajo del que trae algo.
    #[serde(default)]
    pub espacio: Option<String>,
    /// Lo que contestó la conexión cuando se le pidió el perfil del usuario.
    #[serde(default)]
    pub perfil: Option<String>,
    /// Lo que contestó la conexión a la búsqueda que se le pidió.
    #[serde(default)]
    pub pagina: Option<String>,
    /// Por qué no pudo pedirle nada a la conexión.
    #[serde(default)]
    pub falla: Option<String>,
}

/// Qué conversación está leyendo un espacio por su conexión, y cuánto lleva.
#[derive(Debug)]
pub struct Turno {
    sesion: String,
    espacio: String,
    vence: Instant,
    /// Desde qué segundo pide la búsqueda en curso: va igual en todas sus
    /// páginas. Un turno puede llevar varias búsquedas, una detrás de otra.
    despues: i64,
    paginas: usize,
    juntado: Juntado,
    chats: HashSet<String>,
}

/// Con qué clave queda anotada en `ajustes` la última lectura de un espacio.
pub(super) fn clave(espacio: &str) -> String {
    format!("juntada:conexion:{espacio}")
}

/// Con qué clave queda anotado quién es el usuario en un espacio.
fn clave_del_usuario(espacio: &str) -> String {
    format!("slack:{espacio}:usuario")
}

// ---------- lo que contesta la conexión ----------

/// Dónde se escribió un mensaje.
#[derive(Debug, PartialEq, Eq)]
enum Lugar {
    Canal(String),
    Directo,
    Grupo,
}

/// Un mensaje, como lo cuenta la conexión.
#[derive(Debug, PartialEq, Eq)]
struct Encontrado {
    /// Cuándo, en millonésimas de segundo.
    micros: i64,
    /// El identificador de la conversación.
    canal: String,
    lugar: Lugar,
    /// Quiénes están en un mensaje directo o en un grupo: nombre e identificador.
    participantes: Vec<(String, String)>,
    /// El identificador de quien lo escribió.
    autor: String,
    texto: String,
}

/// Una página de resultados de la búsqueda.
#[derive(Debug, Default, PartialEq, Eq)]
struct Pagina {
    mensajes: Vec<Encontrado>,
    /// Cuántos resultados traía, se hayan podido leer o no.
    resultados: usize,
    /// Con qué se pide la página que sigue, si hay.
    cursor: Option<String>,
}

/// El título de cada resultado.
static RESULTADO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^### Result \d+ of \d+[ \t]*$").expect("patrón válido"));
static CANAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Channel: (.+) \(ID: ([A-Z0-9]+)\)[ \t]*$").expect("patrón válido"));
static PARTICIPANTES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Participants: (.+)$").expect("patrón válido"));
/// Cada participante: «Nombre (ID: U…)».
static PERSONA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*([^,]+?) \(ID: ([A-Z0-9]+)\)").expect("patrón válido"));
static AUTOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^From: .*\(ID: ([A-Z0-9]+)\)[ \t]*$").expect("patrón válido"));
static MOMENTO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Message_ts: ([0-9]+(?:\.[0-9]+)?)[ \t]*$").expect("patrón válido"));
/// Donde empieza el texto del mensaje: lo de antes son sus datos.
static TEXTO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^Text:[ \t]*\n").expect("patrón válido"));
static CURSOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"cursor `([^`\s]+)`").expect("patrón válido"));
static USUARIO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^User ID: ([UW][A-Z0-9]+)[ \t]*$").expect("patrón válido"));

/// Un resultado de la búsqueda. `None` si no trae lo que hace falta.
fn leer_resultado(bloque: &str) -> Option<Encontrado> {
    let corte = TEXTO.find(bloque)?;
    // Los datos se buscan solo antes del texto: adentro puede decir cualquier cosa.
    let (datos, texto) = (&bloque[..corte.start()], &bloque[corte.end()..]);
    let canal = CANAL.captures(datos)?;
    let lugar = match canal[1].trim() {
        "DM" => Lugar::Directo,
        "Group DM" => Lugar::Grupo,
        nombre => Lugar::Canal(nombre.trim_start_matches('#').to_owned()),
    };
    let participantes = PARTICIPANTES.captures(datos).map_or_else(Vec::new, |lista| {
        PERSONA.captures_iter(&lista[1]).map(|p| (p[1].trim().to_owned(), p[2].to_owned())).collect()
    });
    // Cada resultado termina con una raya que lo separa del que sigue.
    let texto = texto.trim_end();
    let texto = texto.strip_suffix("---").unwrap_or(texto).trim();
    Some(Encontrado {
        micros: slack::micros_de(&MOMENTO.captures(datos)?[1])?,
        canal: canal[2].to_owned(),
        lugar,
        participantes,
        autor: AUTOR.captures(datos)?[1].to_owned(),
        texto: texto.to_owned(),
    })
}

/// Lo que contestó la conexión a una búsqueda: viene como un objeto con el
/// texto de los resultados y cómo pedir la página que sigue, o como texto suelto.
fn leer_pagina(crudo: &str) -> Pagina {
    let (resultados, paginado) = match serde_json::from_str::<Value>(crudo) {
        Ok(objeto) if objeto.is_object() => (
            objeto["results"].as_str().unwrap_or_default().to_owned(),
            objeto["pagination_info"].as_str().unwrap_or_default().to_owned(),
        ),
        _ => (crudo.to_owned(), crudo.to_owned()),
    };
    let bloques: Vec<&str> = RESULTADO.split(&resultados).skip(1).collect();
    Pagina {
        mensajes: bloques.iter().filter_map(|bloque| leer_resultado(bloque)).collect(),
        resultados: bloques.len(),
        cursor: CURSOR.captures(&paginado).map(|c| c[1].to_owned()),
    }
}

/// Quién es el usuario, según lo que contestó la conexión al pedirle su perfil.
fn usuario_del_perfil(crudo: &str) -> Option<String> {
    let texto = match serde_json::from_str::<Value>(crudo) {
        Ok(objeto) if objeto.is_object() => objeto["result"].as_str().unwrap_or_default().to_owned(),
        _ => crudo.to_owned(),
    };
    USUARIO.captures(&texto).map(|c| c[1].to_owned())
}

/// A quién le escribía el usuario en una conversación.
#[derive(Debug, PartialEq, Eq)]
enum Destino {
    /// A nadie: las notas que se deja a sí mismo y lo que le escribe al
    /// ayudante de Slack. Esa conversación no se lee.
    Nadie,
    /// A alguien de quien la conexión no dijo el nombre.
    SinNombre,
    /// A esa persona, a los demás de ese grupo o a ese canal.
    A(String),
}

fn para_de(mensaje: &Encontrado, usuario: &str, espacio: &str) -> Destino {
    let otros: Vec<&str> =
        mensaje.participantes.iter().filter(|(_, id)| id != usuario).map(|(nombre, _)| nombre.as_str()).collect();
    let nombrados = if otros.is_empty() { Destino::SinNombre } else { Destino::A(otros.join(", ")) };
    match &mensaje.lugar {
        // El mismo canal puede existir en dos espacios: cada uno es otro.
        Lugar::Canal(nombre) => Destino::A(format!("#{nombre} ({espacio})")),
        Lugar::Directo => {
            let solo = otros.is_empty() && !mensaje.participantes.is_empty();
            if solo || mensaje.participantes.iter().any(|(_, id)| id == SLACKBOT) {
                return Destino::Nadie;
            }
            nombrados
        }
        Lugar::Grupo => nombrados,
    }
}

// ---------- el turno ----------

fn turno(e: &Estado) -> MutexGuard<'_, Option<Turno>> {
    e.turno_de_fuente.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Si el turno de ese espacio sigue siendo de esa conversación; de paso se lo renueva.
fn renovar(e: &Estado, sesion: &str, espacio: &str) -> bool {
    let ahora = Instant::now();
    match turno(e).as_mut() {
        Some(t) if t.sesion == sesion && t.espacio == espacio && t.vence > ahora => {
            t.vence = ahora + DURA_EL_TURNO;
            true
        }
        _ => false,
    }
}

/// El espacio que le toca leer a esa conversación, si hay alguno: el que ya
/// venía leyendo, o uno al que le pasó el tiempo y que nadie está leyendo.
async fn tomar(e: &Estado, cfg: &Slack, sesion: &str) -> Resultado<Option<String>> {
    let ahora = Instant::now();
    {
        let actual = turno(e);
        match actual.as_ref() {
            Some(t) if t.vence > ahora && t.sesion == sesion => return Ok(Some(t.espacio.clone())),
            // Lee otra conversación: de a una por vez.
            Some(t) if t.vence > ahora => return Ok(None),
            _ => {}
        }
    }
    for espacio in cfg.espacios.iter().filter(|espacio| espacio.llave == Llave::Conexion) {
        let ultima = fuentes::anotada(&e.pool, &clave(&espacio.nombre)).await?;
        if !fuentes::vencida(ultima.as_ref(), base::ahora()) {
            continue;
        }
        let despues = hasta(e, &espacio.nombre).await? / MICROS;
        let mut actual = turno(e);
        // Mientras se miraba la base pudo tomarlo otra.
        if actual.as_ref().is_some_and(|t| t.vence > Instant::now()) {
            return Ok(None);
        }
        *actual = Some(Turno {
            sesion: sesion.to_owned(),
            espacio: espacio.nombre.clone(),
            vence: Instant::now() + DURA_EL_TURNO,
            despues,
            paginas: 0,
            juntado: Juntado::default(),
            chats: HashSet::new(),
        });
        return Ok(Some(espacio.nombre.clone()));
    }
    Ok(None)
}

/// Termina la lectura de un espacio: suelta el turno y deja anotado qué pasó,
/// que es de donde sale cuándo le toca la próxima.
async fn terminar(e: &Estado, espacio: &str, resultado: Result<(), String>) -> Resultado<()> {
    let suelto = {
        let mut actual = turno(e);
        if actual.as_ref().is_some_and(|t| t.espacio == espacio) { actual.take() } else { None }
    };
    let (bien, resumen) = match (resultado, suelto) {
        (Ok(()), Some(t)) => (
            true,
            format!(
                "Slack («{espacio}», por la conexión de Claude): {} mensajes nuevos (de {} tuyos en {} conversaciones \
                 que había para leer).",
                t.juntado.nuevos,
                t.juntado.propios,
                t.chats.len()
            ),
        ),
        (Ok(()), None) => (true, format!("Slack («{espacio}», por la conexión de Claude): leído.")),
        (Err(motivo), _) => {
            (false, format!("Slack («{espacio}», por la conexión de Claude): no se pudo leer. {motivo}"))
        }
    };
    fuentes::anotar_en(&e.pool, &clave(espacio), &Juntada { cuando: base::ahora(), bien, resumen: resumen.clone() })
        .await?;
    if bien {
        tracing::info!(resumen, "fuente leída");
    } else {
        tracing::warn!(resumen, "no pude leer la fuente");
    }
    Ok(())
}

// ---------- lo guardado ----------

/// Hasta qué momento (en millonésimas de segundo) se leyó un espacio.
async fn hasta(e: &Estado, espacio: &str) -> Resultado<i64> {
    let leido: Option<i64> = sqlx::query_scalar("select hasta from lecturas_propias where fuente = ?1 and origen = ?2")
        .bind(slack::FUENTE)
        .bind(espacio)
        .fetch_optional(&e.pool)
        .await?;
    Ok(leido.unwrap_or(0))
}

async fn usuario_de(e: &Estado, espacio: &str) -> Resultado<Option<String>> {
    Ok(sqlx::query_scalar("select valor from ajustes where clave = ?1")
        .bind(clave_del_usuario(espacio))
        .fetch_optional(&e.pool)
        .await?)
}

/// Los mensajes de una conversación que quedaron como propios.
#[derive(Default)]
struct DelChat {
    para: Option<String>,
    mensajes: Vec<Propio>,
}

/// Guarda de una página lo que escribió el usuario y no estaba leído, y
/// anota hasta dónde se llegó.
async fn guardar_pagina(e: &Estado, cfg: &Slack, espacio: &str, usuario: &str, pagina: &Pagina) -> Resultado<()> {
    let desde = hasta(e, espacio).await?;
    let mut por_chat: HashMap<String, DelChat> = HashMap::new();
    let (mut mayor, mut leidos, mut suyos) = (desde, 0, 0);
    for mensaje in &pagina.mensajes {
        // Lo que no es suyo no entra, aunque la conexión lo devuelva.
        if mensaje.autor != usuario || mensaje.micros <= desde {
            continue;
        }
        mayor = mayor.max(mensaje.micros);
        let para = match para_de(mensaje, usuario, espacio) {
            Destino::Nadie => continue,
            Destino::SinNombre => None,
            Destino::A(nombre) => Some(nombre),
        };
        if excluido(para.as_deref(), &cfg.sin) {
            continue;
        }
        leidos += 1;
        let (Some(texto), Some(fecha)) = (
            propios::lo_propio(&slack::texto_llano(&mensaje.texto), &e.nombre),
            DateTime::<Utc>::from_timestamp_micros(mensaje.micros),
        ) else {
            continue;
        };
        let del_chat = por_chat.entry(format!("slack:{espacio}:{}", mensaje.canal)).or_default();
        del_chat.para = del_chat.para.take().or(para);
        del_chat.mensajes.push(Propio { texto, fecha });
        suyos += 1;
    }
    let mut nuevos = 0;
    for (conversacion, del_chat) in &por_chat {
        nuevos += super::guardar(
            &e.pool,
            slack::FUENTE,
            slack::FUENTE,
            Some(conversacion),
            del_chat.para.as_deref(),
            &del_chat.mensajes,
        )
        .await?;
    }
    if mayor > desde {
        sqlx::query(
            "insert into lecturas_propias (fuente, origen, hasta, tamano) values (?1, ?2, ?3, 0)
             on conflict (fuente, origen) do update set hasta = excluded.hasta",
        )
        .bind(slack::FUENTE)
        .bind(espacio)
        .bind(mayor)
        .execute(&e.pool)
        .await?;
    }
    if let Some(t) = turno(e).as_mut().filter(|t| t.espacio == espacio) {
        t.paginas += 1;
        t.juntado.leidos += leidos;
        t.juntado.propios += suyos;
        t.juntado.nuevos += nuevos;
        t.chats.extend(por_chat.into_keys());
    }
    Ok(())
}

// ---------- lo que se le contesta al mod ----------

/// Lo que hay que hacer después de recibir algo del mod.
enum Sigue {
    /// Pedirle esto a la conexión.
    Pedir(Value),
    /// Decidir qué pedir: lo que trajo no era una página.
    Decidir,
    /// Nada más: la lectura de ese espacio terminó.
    Nada,
}

fn pedir_busqueda(espacio: &str, usuario: &str, despues: i64, cursor: Option<&str>) -> Value {
    json!({
        "espacio": espacio,
        "pedir": "buscar",
        "usuario": usuario,
        // Sin esto se pide desde el principio.
        "despues": (despues > 0).then(|| despues.to_string()),
        "cursor": cursor,
        "pausa_ms": PAUSA_MS,
    })
}

/// Qué pedirle a la conexión para un espacio cuyo turno ya está tomado: el
/// perfil, si todavía no se sabe quién es el usuario ahí; si no, la búsqueda.
async fn que_pedir(e: &Estado, espacio: &str) -> Resultado<Value> {
    let Some(usuario) = usuario_de(e, espacio).await? else {
        return Ok(json!({ "espacio": espacio, "pedir": "perfil" }));
    };
    let despues = turno(e).as_ref().map_or(0, |t| t.despues);
    Ok(pedir_busqueda(espacio, &usuario, despues, None))
}

/// Recibe lo que el mod trae de un espacio cuyo turno tiene.
async fn recibir(e: &Estado, cfg: &Slack, espacio: &str, paso: &PasoDeFuente) -> Resultado<Sigue> {
    if let Some(motivo) = paso.falla.as_deref().map(str::trim).filter(|motivo| !motivo.is_empty()) {
        terminar(e, espacio, Err(motivo.chars().take(LARGO_DEL_MOTIVO).collect())).await?;
        return Ok(Sigue::Nada);
    }
    if let Some(perfil) = paso.perfil.as_deref() {
        let Some(usuario) = usuario_del_perfil(perfil) else {
            terminar(e, espacio, Err("La conexión de Slack no dijo quién es el usuario.".to_owned())).await?;
            return Ok(Sigue::Nada);
        };
        sqlx::query(
            "insert into ajustes (clave, valor) values (?1, ?2)
             on conflict (clave) do update set valor = excluded.valor",
        )
        .bind(clave_del_usuario(espacio))
        .bind(usuario)
        .execute(&e.pool)
        .await?;
        return Ok(Sigue::Decidir);
    }
    let (Some(crudo), Some(usuario)) = (paso.pagina.as_deref(), usuario_de(e, espacio).await?) else {
        return Ok(Sigue::Decidir);
    };
    let pagina = leer_pagina(crudo);
    if pagina.resultados > 0 && pagina.mensajes.is_empty() {
        let motivo = "La conexión de Slack contestó con otra forma que la esperada: puede haber cambiado.";
        terminar(e, espacio, Err(motivo.to_owned())).await?;
        return Ok(Sigue::Nada);
    }
    guardar_pagina(e, cfg, espacio, &usuario, &pagina).await?;
    let (despues, paginas) = turno(e).as_ref().map_or((0, PAGINAS_POR_TURNO), |t| (t.despues, t.paginas));
    if paginas >= PAGINAS_POR_TURNO {
        terminar(e, espacio, Ok(())).await?;
        return Ok(Sigue::Nada);
    }
    if let Some(cursor) = pagina.cursor.as_deref().filter(|_| pagina.resultados > 0) {
        return Ok(Sigue::Pedir(pedir_busqueda(espacio, &usuario, despues, Some(cursor))));
    }
    // La conexión no da más páginas de esta búsqueda, pero corta cada una
    // bastante antes del final: si con esta se avanzó, se busca de nuevo
    // desde donde se llegó. Recién cuando una búsqueda no trae nada más
    // nuevo, no queda nada.
    let llegado = hasta(e, espacio).await? / MICROS;
    if llegado > despues {
        if let Some(t) = turno(e).as_mut().filter(|t| t.espacio == espacio) {
            t.despues = llegado;
        }
        return Ok(Sigue::Pedir(pedir_busqueda(espacio, &usuario, llegado, None)));
    }
    terminar(e, espacio, Ok(())).await?;
    Ok(Sigue::Nada)
}

/// Atiende al mod de una conversación (`sesion`): recibe lo que trae, si trae
/// algo, y le dice qué pedirle ahora a la conexión de Slack de Claude. `None`
/// si no hay nada que pedir: no hay espacios que se lean así, no les toca, o
/// los está leyendo otra conversación.
pub async fn atender(e: &Estado, sesion: &str, paso: &PasoDeFuente) -> Resultado<Option<Value>> {
    let Some(cfg) = e.fuentes.slack.as_ref() else {
        return Ok(None);
    };
    if !paso.conexiones.iter().any(|conexion| conexion.trim().eq_ignore_ascii_case(CONEXION_DE_SLACK)) {
        return Ok(None);
    }
    if let Some(espacio) = paso.espacio.as_deref().map(str::trim).filter(|espacio| !espacio.is_empty()) {
        // Lo que trae vale solo si el turno sigue siendo suyo.
        if !renovar(e, sesion, espacio) {
            return Ok(None);
        }
        match recibir(e, cfg, espacio, paso).await? {
            Sigue::Pedir(pedido) => return Ok(Some(pedido)),
            Sigue::Decidir | Sigue::Nada => {}
        }
    }
    match tomar(e, cfg, sesion).await? {
        Some(espacio) => Ok(Some(que_pedir(e, &espacio).await?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    /// Una página como las que contesta la conexión, con tres resultados: un
    /// mensaje directo, uno en un canal (dentro de un hilo) y uno en un grupo.
    const PAGINA: &str = "# Search Results for: \n\n## Messages (3 results)\n### Result 1 of 3\nChannel: DM (ID: D0LUIS)\nParticipants: Luis Gómez (ID: U0LUIS), Ana Ruiz (ID: U0ANA)\nFrom: Ana Ruiz <ana@example.com> (ID: U0ANA) \nTime: 2026-11-10 09:00:00 -03\nMessage_ts: 1794312000.000100\nPermalink: [link](https://acme.example.com/archives/D0LUIS/p1794312000000100)\nText: \ndale, mañana te mando el resumen\n\n---\n\n### Result 2 of 3\nChannel: #equipo-acme (ID: C0EQUIPO)\nFrom: Ana Ruiz <ana@example.com> (ID: U0ANA) \nTime: 2026-11-10 09:05:00 -03\nMessage_ts: 1794312300.000200\nPermalink: [link](https://acme.example.com/archives/C0EQUIPO/p1794312300000200?thread_ts=1794312000.000001&cid=C0EQUIPO)\nText: \nalguien pudo revisar lo de ayer?\nyo no llegue\n\n---\n\n### Result 3 of 3\nChannel: Group DM (ID: C0GRUPO)\nParticipants: Luis Gómez (ID: U0LUIS), Marta Ruiz (ID: U0MARTA), Ana Ruiz (ID: U0ANA)\nFrom: Ana Ruiz <ana@example.com> (ID: U0ANA) \nTime: 2026-11-10 09:10:00 -03\nMessage_ts: 1794312600.000300\nPermalink: [link](https://acme.example.com/archives/C0GRUPO/p1794312600000300)\nText: \nnos juntamos el jueves?\n\n---\n\n";

    fn como_objeto(resultados: &str, paginado: &str) -> String {
        json!({ "results": resultados, "pagination_info": paginado }).to_string()
    }

    #[test]
    fn lee_lo_que_contesta_la_conexion() {
        let crudo = como_objeto(PAGINA, "For the next page of results use cursor `Q1VSU09S`\n");
        let pagina = leer_pagina(&crudo);
        assert_eq!((pagina.resultados, pagina.cursor.as_deref()), (3, Some("Q1VSU09S")));
        let [directo, canal, grupo] = &pagina.mensajes[..] else { panic!("{pagina:?}") };
        assert_eq!(
            (directo.micros, directo.canal.as_str(), &directo.lugar),
            (1_794_312_000_000_100, "D0LUIS", &Lugar::Directo)
        );
        assert_eq!((directo.autor.as_str(), directo.texto.as_str()), ("U0ANA", "dale, mañana te mando el resumen"));
        assert_eq!(
            directo.participantes,
            [("Luis Gómez".to_owned(), "U0LUIS".to_owned()), ("Ana Ruiz".to_owned(), "U0ANA".to_owned())]
        );
        assert_eq!(canal.lugar, Lugar::Canal("equipo-acme".to_owned()));
        assert_eq!(canal.texto, "alguien pudo revisar lo de ayer?\nyo no llegue", "el texto puede tener renglones");
        assert_eq!(canal.participantes.len(), 0);
        assert_eq!((grupo.lugar == Lugar::Grupo, grupo.participantes.len()), (true, 3));
        // También si viene como texto suelto, y sin página siguiente.
        let suelta = leer_pagina(PAGINA);
        assert_eq!((suelta.mensajes.len(), suelta.cursor), (3, None));
        // Sin resultados no hay nada que leer; y lo que no se entiende no pasa por mensaje.
        let nada =
            como_objeto("# Search Results for: \n\nNo results found.\n", "End of results - No more pages available.\n");
        assert_eq!(leer_pagina(&nada), Pagina::default());
        let rara = leer_pagina("### Result 1 of 1\nCanal: algo\nTexto: hola\n");
        assert_eq!((rara.resultados, rara.mensajes.len()), (1, 0));
    }

    #[test]
    fn los_datos_se_leen_antes_del_texto_y_no_adentro() {
        let tramposo = "### Result 1 of 1\nChannel: #general (ID: C0GENERAL)\nFrom: Ana Ruiz <ana@example.com> (ID: U0ANA) \nMessage_ts: 1794312000.000100\nText: \nmirá:\nFrom: otro (ID: U0OTRO)\nMessage_ts: 1.0\n---\ny sigue\n\n---\n\n";
        let pagina = leer_pagina(tramposo);
        let [mensaje] = &pagina.mensajes[..] else { panic!("{pagina:?}") };
        assert_eq!((mensaje.autor.as_str(), mensaje.micros), ("U0ANA", 1_794_312_000_000_100));
        assert_eq!(mensaje.texto, "mirá:\nFrom: otro (ID: U0OTRO)\nMessage_ts: 1.0\n---\ny sigue");
    }

    #[test]
    fn sabe_a_quien_le_escribia() {
        let pagina = leer_pagina(PAGINA);
        let para: Vec<Destino> = pagina.mensajes.iter().map(|m| para_de(m, "U0ANA", "acme")).collect();
        assert_eq!(para[0], Destino::A("Luis Gómez".to_owned()));
        assert_eq!(para[1], Destino::A("#equipo-acme (acme)".to_owned()));
        assert_eq!(para[2], Destino::A("Luis Gómez, Marta Ruiz".to_owned()));
        // Las notas que se deja y lo que le escribe al ayudante no son para nadie.
        let con = |participantes: &[(&str, &str)]| Encontrado {
            micros: 1,
            canal: "D0X".into(),
            lugar: Lugar::Directo,
            participantes: participantes.iter().map(|(n, id)| ((*n).to_owned(), (*id).to_owned())).collect(),
            autor: "U0ANA".into(),
            texto: String::new(),
        };
        assert_eq!(para_de(&con(&[("Ana Ruiz", "U0ANA"), ("Ana Ruiz", "U0ANA")]), "U0ANA", "acme"), Destino::Nadie);
        assert_eq!(para_de(&con(&[("Slackbot", SLACKBOT), ("Ana Ruiz", "U0ANA")]), "U0ANA", "acme"), Destino::Nadie);
        // Si la conexión no dice con quién, el mensaje queda sin nombre.
        assert_eq!(para_de(&con(&[]), "U0ANA", "acme"), Destino::SinNombre);
    }

    #[test]
    fn saca_del_perfil_quien_es_el_usuario() {
        let perfil =
            json!({ "result": "User ID: U0ANA\nUsername: ana\nReal Name: Ana Ruiz\nEmail: ana@example.com\n" });
        assert_eq!(usuario_del_perfil(&perfil.to_string()).as_deref(), Some("U0ANA"));
        assert_eq!(usuario_del_perfil("User ID: W0ANA\nUsername: ana\n").as_deref(), Some("W0ANA"));
        assert_eq!(usuario_del_perfil("Username: ana\n"), None);
    }
}
