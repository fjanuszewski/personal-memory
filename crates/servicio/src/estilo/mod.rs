//! Estilo del usuario (punto 4.11 del diseño): la memoria junta lo que él
//! escribe, mide cómo lo escribe y se lo cuenta a Claude cuando tiene que
//! redactar algo que va a salir con su nombre.
//!
//! Tres partes: de dónde salen los mensajes (sus conversaciones con Claude,
//! en [`propios`], y las fuentes que él encienda: [`whatsapp`] y [`slack`]),
//! qué se mide en ellos ([`numeros`]) y lo que responde la herramienta
//! `memoria_estilo` ([`consultar`]). Todo queda en la
//! base de esta máquina y no hay ningún modelo de por medio: son cuentas.

pub mod conexion;
pub mod fuentes;
pub mod idioma;
pub mod lexico;
pub mod numeros;
pub mod propios;
pub mod slack;
pub mod whatsapp;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Local, Utc};
use sqlx::SqlitePool;

use crate::error::{Resultado, uso};
use crate::estado::Estado;
use crate::grupos::{self, Grupo};
use crate::lector::{self, Ubicaciones};
use crate::notas::{TAPADO, tapar_secretos};
use crate::{base, texto};

use self::numeros::Medido;
use self::propios::Propio;

/// La fuente de los mensajes escritos en conversaciones con Claude, y el
/// lugar con que quedan guardados.
pub const FUENTE_CLAUDE: &str = "claude";
/// Para armar el perfil se miran los mensajes más nuevos, hasta esta cantidad.
const MAXIMO_PARA_MEDIR: i64 = 3000;
const EJEMPLOS_POR_DEFECTO: usize = 6;
const EJEMPLOS_MAXIMOS: usize = 15;
/// Un ejemplo sirve si no es un monosílabo ni un texto largo…
const EJEMPLO_MINIMO: usize = 25;
const EJEMPLO_MAXIMO: usize = 300;
/// …y si es una frase corrida, de pocos renglones.
const RENGLONES_DE_EJEMPLO: usize = 3;
/// Con menos mensajes que estos no se arma un perfil.
const MINIMO_PARA_PERFIL: usize = 5;
/// Con menos mensajes que estos, el perfil es apenas una pista; y hacen
/// falta por lo menos estos a una persona para medir cómo le escribe a ella.
const POCOS: usize = 30;
/// Cuántas horas de silencio en una conversación separan una charla de la
/// siguiente: después de eso, se vuelve a saludar.
const HORAS_ENTRE_CHARLAS: i64 = 6;
/// Cuántos chats se nombran cuando un nombre coincide con varios.
const CHATS_NOMBRADOS: usize = 5;

/// Guarda mensajes del usuario; los que ya estaban no se repiten. Devuelve
/// cuántos eran nuevos.
pub async fn guardar(
    pool: &SqlitePool,
    fuente: &str,
    lugar: &str,
    conversacion: Option<&str>,
    para: Option<&str>,
    mensajes: &[Propio],
) -> Resultado<usize> {
    if mensajes.is_empty() {
        return Ok(0);
    }
    let mut tx = base::escribir(pool).await?;
    let mut nuevos = 0;
    for mensaje in mensajes {
        let contenido = tapar_secretos(&mensaje.texto);
        let hecho = sqlx::query(
            "insert or ignore into mensajes_propios (fuente, lugar, conversacion, para, idioma, fecha, contenido, marca)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(fuente)
        .bind(lugar)
        .bind(conversacion)
        .bind(para)
        .bind(idioma::de(&contenido))
        .bind(mensaje.fecha)
        .bind(contenido.as_ref())
        .bind(texto::marca(&contenido))
        .execute(&mut *tx)
        .await?;
        nuevos += hecho.rows_affected();
    }
    tx.commit().await?;
    Ok(usize::try_from(nuevos).unwrap_or(usize::MAX))
}

fn posicion(valor: u64) -> i64 {
    i64::try_from(valor).unwrap_or(i64::MAX)
}

/// Lee lo nuevo que escribió el usuario en una conversación y lo guarda.
async fn leer_conversacion(
    e: &Estado,
    conversacion: &str,
    transcripcion: &str,
    archivo: &Path,
    desde: u64,
    tamano: u64,
) -> anyhow::Result<usize> {
    let ruta = archivo.to_owned();
    let nombre = e.nombre.clone();
    let lectura = tokio::task::spawn_blocking(move || propios::leer(&ruta, desde, &nombre))
        .await
        .context("se cortó la lectura")?
        .with_context(|| format!("no pude leer {}", archivo.display()))?;
    let nuevos = guardar(&e.pool, FUENTE_CLAUDE, FUENTE_CLAUDE, Some(conversacion), None, &lectura.mensajes).await?;
    sqlx::query(
        "insert into lecturas_propias (fuente, origen, hasta, tamano) values (?1, ?2, ?3, ?4)
         on conflict (fuente, origen) do update set hasta = excluded.hasta, tamano = excluded.tamano",
    )
    .bind(FUENTE_CLAUDE)
    .bind(transcripcion)
    .bind(posicion(lectura.hasta))
    .bind(posicion(tamano))
    .execute(&e.pool)
    .await?;
    Ok(nuevos)
}

/// Revisa las conversaciones registradas y junta lo que el usuario escribió
/// desde la última vez. Solo mira las que cambiaron y llevan `reposo` quietas:
/// para entender un mensaje hay que releer lo que Claude mostró antes, y no
/// vale la pena hacerlo mientras el archivo sigue creciendo. Devuelve cuántos
/// mensajes nuevos guardó.
pub async fn recorrer(e: &Estado, raiz: &Path, conocidos: &mut Ubicaciones, reposo: Duration) -> anyhow::Result<usize> {
    let conversaciones: Vec<(String, String)> = sqlx::query_as(
        "select id, transcripcion from conversaciones where transcripcion is not null order by actualizada desc",
    )
    .fetch_all(&e.pool)
    .await?;
    let leidas: Vec<(String, i64, i64)> =
        sqlx::query_as("select origen, hasta, tamano from lecturas_propias where fuente = ?1")
            .bind(FUENTE_CLAUDE)
            .fetch_all(&e.pool)
            .await?;
    let leidas: HashMap<String, (i64, i64)> =
        leidas.into_iter().map(|(o, hasta, tamano)| (o, (hasta, tamano))).collect();
    let mut nuevos = 0;
    for (conversacion, transcripcion) in conversaciones {
        let Some(archivo) = lector::ubicar(raiz, &transcripcion, conocidos) else { continue };
        let Ok(datos) = std::fs::metadata(&archivo) else { continue };
        let tamano = datos.len();
        let (hasta, visto) = leidas.get(&transcripcion).copied().unwrap_or((0, 0));
        if u64::try_from(visto).ok() == Some(tamano) {
            continue;
        }
        let quieto = datos.modified().ok().and_then(|m| m.elapsed().ok()).is_none_or(|rato| rato >= reposo);
        if !quieto {
            continue;
        }
        // Si el archivo es más corto que lo ya leído, es otro: desde el principio.
        let desde = u64::try_from(hasta).ok().filter(|h| *h <= tamano).unwrap_or(0);
        match leer_conversacion(e, &conversacion, &transcripcion, &archivo, desde, tamano).await {
            Ok(cuantos) => nuevos += cuantos,
            Err(error) => {
                tracing::warn!(conversacion = %conversacion, error = format!("{error:#}"), "no pude leer los mensajes del usuario");
            }
        }
    }
    if nuevos > 0 {
        tracing::info!(nuevos, "mensajes del usuario guardados para aprender cómo escribe");
    }
    Ok(nuevos)
}

/// Lo que se le pide a `memoria_estilo`.
#[derive(Debug, Default)]
pub struct Pedido<'a> {
    /// Dónde va a salir lo que se va a escribir.
    pub lugar: Option<&'a str>,
    pub idioma: Option<&'a str>,
    /// A quién se le va a escribir: el nombre de la persona o del grupo.
    pub para: Option<&'a str>,
    /// De qué trata.
    pub sobre: Option<&'a str>,
    pub ejemplos: Option<usize>,
    /// El grupo de la conversación que pregunta: sus ejemplos van primero.
    pub grupo: Option<&'a Grupo>,
}

/// El nombre de un lugar dicho de cualquier forma: «Mail», «e-mail» y
/// «correo» son el mismo.
#[must_use]
pub fn leer_lugar(texto: &str) -> String {
    let plano = idioma::plano(texto.trim());
    match plano.as_str() {
        "mail" | "mails" | "email" | "e-mail" | "correo" | "correos" | "gmail" | "outlook" => "correo".to_owned(),
        "wsp" | "wa" | "wapp" | "whatsapp" | "whats app" => "whatsapp".to_owned(),
        "chat" | "claude" | "chat con claude" => FUENTE_CLAUDE.to_owned(),
        _ => plano,
    }
}

struct Mensaje {
    lugar: String,
    idioma: String,
    /// Si se lo mandó a alguien (por una mensajería, por correo) y no a Claude.
    dirigido: bool,
    /// Dónde lo escribió, si se sabe: una conversación con Claude o un chat.
    conversacion: Option<String>,
    para: Option<String>,
    fecha: DateTime<Utc>,
    contenido: String,
    grupo_id: Option<i64>,
    /// El nombre del grupo de su conversación, si tiene.
    grupo: Option<String>,
}

type Fila = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    DateTime<Utc>,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
);

/// Cuántos mensajes hay de cada lugar y de cada idioma.
async fn cuentas(pool: &SqlitePool) -> Resultado<Vec<(String, String, usize)>> {
    let filas: Vec<(String, String, i64)> = sqlx::query_as(
        "select lugar, idioma, count(*) from mensajes_propios group by lugar, idioma order by count(*) desc, lugar, idioma",
    )
    .fetch_all(pool)
    .await?;
    Ok(filas.into_iter().map(|(l, i, n)| (l, i, usize::try_from(n).unwrap_or(0))).collect())
}

/// Los mensajes más nuevos de un lugar (o de todos, si es `None`) en un
/// idioma; con `con_cortos`, también los que son muy cortos para tener
/// idioma. Con `para`, solo los que le mandó a esa persona o a ese grupo.
async fn mensajes(
    pool: &SqlitePool,
    lugar: Option<&str>,
    idioma: &str,
    con_cortos: bool,
    para: Option<&str>,
) -> Resultado<Vec<Mensaje>> {
    let filas: Vec<Fila> = sqlx::query_as(
        "select m.fuente, m.lugar, m.idioma, m.conversacion, m.para, m.fecha, m.contenido, c.grupo_id, g.empresa,
                g.proyecto
         from mensajes_propios m
         left join conversaciones c on m.fuente = 'claude' and c.id = m.conversacion
         left join grupos g on g.id = c.grupo_id
         where (?1 is null or m.lugar = ?1) and (m.idioma = ?2 or (?3 and m.idioma = ''))
           and (?4 is null or m.para = ?4)
         order by m.fecha desc, m.id desc limit ?5",
    )
    .bind(lugar)
    .bind(idioma)
    .bind(con_cortos)
    .bind(para)
    .bind(MAXIMO_PARA_MEDIR)
    .fetch_all(pool)
    .await?;
    Ok(filas
        .into_iter()
        .map(|(fuente, lugar, idioma, conversacion, para, fecha, contenido, grupo_id, empresa, proyecto)| Mensaje {
            lugar,
            idioma,
            dirigido: fuente != FUENTE_CLAUDE,
            conversacion,
            para,
            fecha,
            contenido,
            grupo_id,
            grupo: empresa.map(|e| grupos::nombre(&e, proyecto.as_deref().filter(|p| !p.is_empty()))),
        })
        .collect())
}

/// «claude (312), whatsapp (40)».
fn lugares(cuentas: &[(String, String, usize)]) -> String {
    let mut por_lugar: Vec<(&str, usize)> = Vec::new();
    for (lugar, _, cuantos) in cuentas {
        match por_lugar.iter_mut().find(|(l, _)| l == lugar) {
            Some((_, total)) => *total += cuantos,
            None => por_lugar.push((lugar, *cuantos)),
        }
    }
    por_lugar.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    por_lugar.iter().map(|(lugar, cuantos)| format!("{lugar} ({cuantos})")).collect::<Vec<_>>().join(", ")
}

/// El idioma con más mensajes en un lugar (o en todos). Vacío si todos los
/// mensajes son muy cortos para saberlo.
fn idioma_principal(cuentas: &[(String, String, usize)], lugar: Option<&str>) -> String {
    let mut por_idioma: HashMap<&str, usize> = HashMap::new();
    for (l, idioma, cuantos) in cuentas {
        if lugar.is_none_or(|pedido| pedido == l) && !idioma.is_empty() {
            *por_idioma.entry(idioma).or_default() += cuantos;
        }
    }
    let mut lista: Vec<(&str, usize)> = por_idioma.into_iter().collect();
    lista.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    lista.first().map_or_else(String::new, |(idioma, _)| (*idioma).to_owned())
}

/// A quién le va a escribir, entre las personas y los grupos a los que el
/// usuario ya les escribió.
#[derive(Debug, PartialEq, Eq)]
enum Destinatario {
    /// No se dijo a quién.
    SinDecir,
    /// Nunca le escribió, o no con ese nombre.
    Desconocido,
    /// Su nombre, como figura en los chats.
    Uno(String),
    /// El nombre coincide con varios chats: los que más mensajes tienen.
    Varios(Vec<String>),
}

/// Las personas y los grupos a los que les escribió en un lugar (o en todos),
/// con cuántos mensajes a cada uno.
async fn chats(pool: &SqlitePool, lugar: Option<&str>) -> Resultado<Vec<(String, usize)>> {
    let filas: Vec<(String, i64)> = sqlx::query_as(
        "select para, count(*) from mensajes_propios
         where para is not null and (?1 is null or lugar = ?1) group by para",
    )
    .bind(lugar)
    .fetch_all(pool)
    .await?;
    Ok(filas.into_iter().map(|(para, n)| (para, usize::try_from(n).unwrap_or(0))).collect())
}

/// Busca a quién se refiere `dicho` entre los nombres de los chats. Vale el
/// nombre entero, o que todas sus palabras estén enteras en el nombre: «Ana»
/// es «Ana Trabajo», pero no «Mariana». Si coincide con más de uno no elige:
/// lo que se le escribió a una persona no se muestra a nombre de otra.
fn reconocer(dicho: &str, chats: &[(String, usize)]) -> Destinatario {
    let pedidas = idioma::palabras(dicho);
    if pedidas.is_empty() {
        return Destinatario::SinDecir;
    }
    let mut coinciden: Vec<(&str, usize, bool)> = chats
        .iter()
        .filter_map(|(nombre, cuantos)| {
            let suyas = idioma::palabras(nombre);
            pedidas.iter().all(|pedida| suyas.contains(pedida)).then(|| (nombre.as_str(), *cuantos, suyas == pedidas))
        })
        .collect();
    coinciden.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    if let Some((exacto, ..)) = coinciden.iter().find(|(.., exacto)| *exacto) {
        return Destinatario::Uno((*exacto).to_owned());
    }
    match coinciden[..] {
        [] => Destinatario::Desconocido,
        [(unico, ..)] => Destinatario::Uno(unico.to_owned()),
        _ => Destinatario::Varios(
            coinciden.iter().take(CHATS_NOMBRADOS).map(|(nombre, ..)| (*nombre).to_owned()).collect(),
        ),
    }
}

/// El idioma en que más le escribe a alguien. Vacío si todo lo que le mandó
/// es muy corto para saberlo.
async fn idioma_con(pool: &SqlitePool, lugar: Option<&str>, para: &str) -> Resultado<String> {
    let idioma: Option<String> = sqlx::query_scalar(
        "select idioma from mensajes_propios
         where para = ?2 and (?1 is null or lugar = ?1) and idioma <> ''
         group by idioma order by count(*) desc, idioma limit 1",
    )
    .bind(lugar)
    .bind(para)
    .fetch_optional(pool)
    .await?;
    Ok(idioma.unwrap_or_default())
}

/// Marca con cuáles mensajes el usuario empezó o terminó una charla: los que
/// tienen horas de silencio antes o después en su conversación. Vienen del
/// más nuevo al más viejo. `completos` dice si están todos y no solo los más
/// nuevos: recién ahí el más viejo de cada conversación es un comienzo.
fn charlas(mensajes: &[Mensaje], completos: bool, ahora: DateTime<Utc>) -> Vec<Medido<'_>> {
    let pausa = chrono::Duration::hours(HORAS_ENTRE_CHARLAS);
    let mut medidos: Vec<Medido<'_>> =
        mensajes.iter().map(|m| Medido { texto: &m.contenido, abre: false, cierra: false }).collect();
    // De cada conversación, el último que se vio: el que le sigue en el tiempo.
    let mut siguiente: HashMap<&str, usize> = HashMap::new();
    for (i, m) in mensajes.iter().enumerate() {
        let Some(conversacion) = m.conversacion.as_deref() else {
            medidos[i] = Medido::suelto(&m.contenido);
            continue;
        };
        match siguiente.insert(conversacion, i) {
            // El más nuevo de su conversación: la terminó si después no escribió más.
            None => medidos[i].cierra = ahora - m.fecha >= pausa,
            Some(posterior) => {
                let silencio = mensajes[posterior].fecha - m.fecha >= pausa;
                medidos[posterior].abre = silencio;
                medidos[i].cierra = silencio;
            }
        }
    }
    if completos {
        for mas_viejo in siguiente.into_values() {
            medidos[mas_viejo].abre = true;
        }
    }
    medidos
}

/// Si una palabra parece un dato y no algo dicho: una cuenta, un teléfono,
/// una clave.
fn parece_un_dato(palabra: &str) -> bool {
    let digitos = palabra.chars().filter(char::is_ascii_digit).count();
    let letras = palabra.chars().filter(|c| c.is_alphabetic()).count();
    digitos >= 5 || (digitos >= 3 && letras >= 3 && palabra.chars().count() >= 8)
}

/// Si un mensaje sirve para mostrar el tono: una frase corrida, ni muy corta
/// ni larga, sin direcciones, sin nada tapado y sin nada que parezca un dato.
/// Lo que se muestra de ejemplo puede aparecer en otra conversación, así que
/// acá se es más estricto que para medir.
fn sirve_de_ejemplo(contenido: &str) -> bool {
    (EJEMPLO_MINIMO..=EJEMPLO_MAXIMO).contains(&contenido.chars().count())
        && !contenido.contains("\n\n")
        && contenido.lines().count() <= RENGLONES_DE_EJEMPLO
        && !contenido.contains('@')
        && !contenido.contains("://")
        && !contenido.contains(TAPADO)
        && !contenido.split_whitespace().any(parece_un_dato)
}

/// Elige los mensajes que mejor sirven de ejemplo entre los que se pueden
/// mostrar. Los del grupo de la conversación que pregunta van siempre antes
/// que los de otros, que solo completan si faltan. Dentro de cada parte:
/// primero lo que habla de lo mismo, y entre iguales, el orden en que vienen.
fn elegir<'a>(mensajes: &[&'a Mensaje], p: &Pedido<'_>, idioma: &str, cuantos: usize) -> Vec<&'a Mensaje> {
    let sobre = p.sobre.map(texto::Consulta::de);
    let mut candidatos: Vec<(bool, usize, usize, &Mensaje)> = mensajes
        .iter()
        .enumerate()
        .filter(|(_, m)| m.idioma == idioma && sirve_de_ejemplo(&m.contenido))
        .map(|(orden, m)| {
            let mut puntos = 0;
            if let Some(sobre) = &sobre {
                let indexado = texto::para_indexar(&m.contenido);
                puntos += sobre.coinciden(&texto::conjunto(&indexado));
            }
            let de_otro_grupo = p.grupo.is_some_and(|grupo| m.grupo_id != Some(grupo.id));
            (de_otro_grupo, puntos, orden, *m)
        })
        .collect();
    candidatos.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)).then_with(|| a.2.cmp(&b.2)));
    let mut vistos: HashSet<String> = HashSet::new();
    candidatos
        .into_iter()
        .filter(|(_, _, _, m)| vistos.insert(idioma::plano(&m.contenido)))
        .take(cuantos)
        .map(|(_, _, _, m)| m)
        .collect()
}

fn dia(fecha: DateTime<Utc>) -> String {
    fecha.with_timezone(&Local).format("%Y-%m-%d").to_string()
}

/// Un ejemplo en un renglón: de cuándo, de dónde, para quién y qué decía.
fn ejemplo(m: &Mensaje, numero: usize, p: &Pedido<'_>) -> String {
    let mut datos = vec![dia(m.fecha), m.lugar.clone()];
    if let Some(para) = m.para.as_deref().filter(|a| !a.trim().is_empty()) {
        datos.push(format!("a {para}"));
    }
    if let Some(grupo) = &m.grupo
        && p.grupo.is_none_or(|pedido| Some(pedido.id) != m.grupo_id)
    {
        datos.push(format!("de {grupo}"));
    }
    let renglones: Vec<&str> = m.contenido.lines().map(str::trim).filter(|renglon| !renglon.is_empty()).collect();
    format!("{numero}. [{}] «{}»", datos.join(" · "), renglones.join(" / "))
}

const SIN_MENSAJES: &str = "Todavía no aprendí cómo escribe el usuario: no tengo mensajes suyos. La memoria los \
junta sola de sus conversaciones, a medida que escribe. Por ahora redactá con un tono neutro y preguntale si quiere \
cambiar algo.";

/// Lo que hay que saber de a quién le va a escribir, si se dijo: que lo
/// medido no es de esa persona, que no se sabe quién es, o cuál de varias.
fn sobre_el_destinatario(d: &Destinatario, dicho: &str, suyos: usize, medido_con_el: bool) -> Option<String> {
    match d {
        Destinatario::SinDecir => None,
        Destinatario::Uno(_) if medido_con_el => None,
        Destinatario::Uno(nombre) if suyos == 0 => Some(format!(
            "A «{nombre}» no le escribió en este idioma: lo de arriba es cómo escribe en general, no cómo le escribe \
             a esa persona."
        )),
        Destinatario::Uno(nombre) => Some(format!(
            "A «{nombre}» le escribió poco ({suyos} mensajes): lo de arriba es cómo escribe en general, no cómo le \
             escribe a esa persona."
        )),
        Destinatario::Desconocido => Some(format!(
            "No encontré a «{dicho}» entre las personas y los grupos a los que les escribió: lo de arriba es cómo \
             escribe en general. Si en sus chats figura con otro nombre, pasá ese en `para`."
        )),
        Destinatario::Varios(nombres) => Some(format!(
            "«{dicho}» coincide con varios chats suyos: {}. Pasá en `para` el nombre completo del que sea; mientras \
             tanto, lo de arriba es cómo escribe en general.",
            nombres.iter().map(|nombre| format!("«{nombre}»")).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Lo que se junta para responder un pedido: de qué lugar, a quién, en qué
/// idioma, y los mensajes de donde sale la respuesta.
struct Reunido {
    /// El lugar que se pidió, haya o no mensajes de ahí.
    pedido: Option<String>,
    /// Ese lugar, si hay mensajes suyos de ahí; si no, van todos juntos.
    lugar: Option<String>,
    destinatario: Destinatario,
    idioma: String,
    /// Los mensajes más nuevos del lugar.
    general: Vec<Mensaje>,
    /// Los que le mandó a la persona o al grupo a quien le va a escribir.
    suyos: Vec<Mensaje>,
}

impl Reunido {
    /// Si ya le escribió bastante a esa persona como para medir cómo le escribe a ella.
    fn alcanza_con_los_suyos(&self) -> bool {
        self.suyos.len() >= POCOS
    }

    /// Los mensajes sobre los que se mide.
    fn medidos(&self) -> &[Mensaje] {
        if self.alcanza_con_los_suyos() { &self.suyos } else { &self.general }
    }
}

async fn reunir(pool: &SqlitePool, p: &Pedido<'_>, cuentas: &[(String, String, usize)]) -> Resultado<Reunido> {
    let pedido = p.lugar.map(leer_lugar).filter(|l| !l.is_empty());
    let lugar = pedido.clone().filter(|l| cuentas.iter().any(|(conocido, _, _)| conocido == l));
    let lugar_de = lugar.as_deref();
    let destinatario = match p.para.map(str::trim).filter(|t| !t.is_empty()) {
        Some(dicho) => reconocer(dicho, &chats(pool, lugar_de).await?),
        None => Destinatario::SinDecir,
    };
    // El idioma: el que se pidió; si no, el que usa con esa persona; si no, el del lugar.
    let del_lugar = idioma_principal(cuentas, lugar_de);
    let con_el = match &destinatario {
        Destinatario::Uno(nombre) => Some(idioma_con(pool, lugar_de, nombre).await?).filter(|i| !i.is_empty()),
        _ => None,
    };
    let idioma = match p.idioma.map(str::trim).filter(|i| !i.is_empty()) {
        None => con_el.clone().unwrap_or_else(|| del_lugar.clone()),
        Some(dicho) => match idioma::leer(dicho) {
            Some(codigo) => codigo.to_owned(),
            None => return uso(format!("No conozco el idioma «{dicho}»: pasá «es», «en» o «pt».")),
        },
    };
    // Lo muy corto para tener idioma se mide junto con el idioma que más usa ahí (o con esa persona).
    let general = mensajes(pool, lugar_de, &idioma, idioma == del_lugar, None).await?;
    let suyos = match &destinatario {
        Destinatario::Uno(nombre) => {
            let principal = con_el.as_deref().unwrap_or(&del_lugar);
            mensajes(pool, lugar_de, &idioma, idioma == principal, Some(nombre)).await?
        }
        _ => Vec::new(),
    };
    Ok(Reunido { pedido, lugar, destinatario, idioma, general, suyos })
}

/// Los ejemplos, o por qué no se muestra ninguno. De ejemplo va lo que le
/// mandó a esa persona y lo que le escribió a Claude; lo que le mandó a otros
/// es de esas charlas y no se muestra.
fn escribir_ejemplos(salida: &mut String, r: &Reunido, p: &Pedido<'_>) {
    let candidatos: Vec<&Mensaje> = r.suyos.iter().chain(r.general.iter().filter(|m| !m.dirigido)).collect();
    let cuantos = p.ejemplos.unwrap_or(EJEMPLOS_POR_DEFECTO).min(EJEMPLOS_MAXIMOS);
    let elegidos = elegir(&candidatos, p, &r.idioma, cuantos);
    if !elegidos.is_empty() {
        let _ = writeln!(salida, "\nMensajes suyos, para tomar el tono (no copies lo que dicen):");
        for (i, m) in elegidos.iter().enumerate() {
            let _ = writeln!(salida, "{}", ejemplo(m, i + 1, p));
        }
    } else if r.general.iter().any(|m| m.dirigido) {
        let como = if r.destinatario == Destinatario::SinDecir {
            " Decí a quién le va a escribir (`para`: el nombre de la persona o del grupo, como figura en sus chats)."
        } else {
            ""
        };
        let _ = writeln!(
            salida,
            "\nNo muestro mensajes suyos de ejemplo: lo que le mandó a alguien se ve solo cuando se sabe a quién le \
             va a escribir y ya le escribió antes.{como}"
        );
    }
}

fn responder(r: &Reunido, p: &Pedido<'_>, cuentas: &[(String, String, usize)]) -> String {
    let medidos = r.medidos();
    let donde = r.lugar.clone().unwrap_or_else(|| "todos los lugares juntos".to_owned());
    let mut salida = String::new();
    if let (Some(pedido), None) = (&r.pedido, &r.lugar) {
        let _ = writeln!(
            salida,
            "No tengo mensajes suyos de «{pedido}»: lo de abajo sale de {}. Sirve para el vocabulario y el tono; \
             las tildes, los signos y los saludos pueden ser otros ahí, así que ante la duda escribí con cuidado.\n",
            lugares(cuentas)
        );
    }
    if medidos.len() < MINIMO_PARA_PERFIL {
        let cuantos = match medidos.len() {
            0 => "No tengo mensajes suyos".to_owned(),
            1 => "Tengo un solo mensaje suyo".to_owned(),
            n => format!("Tengo solo {n} mensajes suyos"),
        };
        let no_alcanza = if medidos.is_empty() { "" } else { ": no alcanza para saber cómo escribe ahí" };
        let _ = write!(
            salida,
            "{cuantos} en {} ({donde}){no_alcanza}. Tengo de: {}.",
            idioma::nombre(&r.idioma),
            lugares(cuentas)
        );
        return salida;
    }
    let completos = i64::try_from(medidos.len()).is_ok_and(|n| n < MAXIMO_PARA_MEDIR);
    let numeros = numeros::medir(&charlas(medidos, completos, base::ahora()), &r.idioma);
    let quien = match &r.destinatario {
        Destinatario::Uno(nombre) if r.alcanza_con_los_suyos() => {
            format!("Así le escribe el usuario a «{nombre}» en {donde}")
        }
        _ => format!("Así escribe el usuario en {donde}"),
    };
    let en = if r.idioma.is_empty() { String::new() } else { format!(" · {}", idioma::nombre(&r.idioma)) };
    let _ = writeln!(
        salida,
        "{quien}{en} ({} mensajes suyos, del {} al {}):",
        medidos.len(),
        medidos.last().map(|m| dia(m.fecha)).unwrap_or_default(),
        medidos.first().map(|m| dia(m.fecha)).unwrap_or_default()
    );
    for linea in numeros::describir(&numeros, &r.idioma) {
        let _ = writeln!(salida, "- {linea}");
    }
    if medidos.len() < POCOS {
        let _ = writeln!(salida, "Son pocos mensajes: tomalo como una pista, no como una regla.");
    }
    let dicho = p.para.map(str::trim).unwrap_or_default();
    if let Some(aviso) = sobre_el_destinatario(&r.destinatario, dicho, r.suyos.len(), r.alcanza_con_los_suyos()) {
        let _ = writeln!(salida, "\n{aviso}");
    }
    escribir_ejemplos(&mut salida, r, p);
    let _ = write!(
        salida,
        "\nEscribí como él: el mismo largo, las mismas mayúsculas, tildes y signos, y sus expresiones. Vale para lo \
         que va a salir con su nombre; con él seguí hablando como siempre. Tengo mensajes suyos de: {}.",
        lugares(cuentas)
    );
    salida
}

/// Lo que responde `memoria_estilo`: cómo escribe el usuario en un lugar y
/// un idioma (o cómo le escribe a una persona, si ya le escribió bastante), y
/// mensajes suyos de ejemplo.
pub async fn consultar(pool: &SqlitePool, p: &Pedido<'_>) -> Resultado<String> {
    let cuentas = cuentas(pool).await?;
    if cuentas.is_empty() {
        return Ok(SIN_MENSAJES.to_owned());
    }
    let reunido = reunir(pool, p, &cuentas).await?;
    Ok(responder(&reunido, p, &cuentas))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn de_ejemplo_solo_van_frases_limpias() {
        assert!(sirve_de_ejemplo("no lo subas todavia, esta bien asi, lo vemos despues"));
        assert!(sirve_de_ejemplo("ok pero cuando lo subimos? tenes una fecha para la version 0.6.5?"));
        for no in [
            "si dale",
            "no me lo toma. le puse .Abc246810x y nada",
            "el numero de cuenta es 123456789, fijate si es ese",
            "escribile a ana@example.com y contale como viene",
            "mirá https://example.com/tablero y decime que ves",
            "Aca tenes el token: [secreto quitado] guardalo bien",
            "primer párrafo de un mensaje largo\n\nsegundo párrafo del mismo mensaje",
            "uno\ndos\ntres\ncuatro renglones son muchos para un ejemplo",
        ] {
            assert!(!sirve_de_ejemplo(no), "{no}");
        }
        assert!(!sirve_de_ejemplo(&"palabra ".repeat(60)));
    }

    #[test]
    fn reconoce_a_quien_se_le_escribe_por_su_nombre_entero() {
        let chats: Vec<(String, usize)> =
            [("Ana Trabajo", 40), ("Mariana", 90), ("Equipo ACME", 12), ("Luis Pérez", 7), ("Luis Gómez", 30)]
                .into_iter()
                .map(|(nombre, cuantos)| (nombre.to_owned(), cuantos))
                .collect();
        // «Ana» es «Ana Trabajo», no «Mariana», aunque a Mariana le escribió más.
        assert_eq!(reconocer("ana", &chats), Destinatario::Uno("Ana Trabajo".into()));
        assert_eq!(reconocer(" MARIANA ", &chats), Destinatario::Uno("Mariana".into()));
        assert_eq!(reconocer("el equipo de acme", &chats), Destinatario::Desconocido, "sobran palabras");
        assert_eq!(reconocer("acme equipo", &chats), Destinatario::Uno("Equipo ACME".into()));
        assert_eq!(reconocer("luis perez", &chats), Destinatario::Uno("Luis Pérez".into()));
        // Con más de uno no elige: los nombra, primero aquel al que más le escribió.
        assert_eq!(reconocer("Luis", &chats), Destinatario::Varios(vec!["Luis Gómez".into(), "Luis Pérez".into()]));
        assert_eq!(reconocer("Marta", &chats), Destinatario::Desconocido);
        assert_eq!(reconocer(" ¿? ", &chats), Destinatario::SinDecir);
        // El nombre entero gana aunque otro chat también lo contenga.
        let con_grupo = [("Ana".to_owned(), 3), ("Ana y Luis".to_owned(), 50)];
        assert_eq!(reconocer("Ana", &con_grupo), Destinatario::Uno("Ana".into()));
    }

    fn mensaje(conversacion: Option<&str>, hace_horas: i64, contenido: &str) -> Mensaje {
        Mensaje {
            lugar: "whatsapp".into(),
            idioma: "es".into(),
            dirigido: true,
            conversacion: conversacion.map(Into::into),
            para: None,
            fecha: DateTime::UNIX_EPOCH + chrono::Duration::hours(1000 - hace_horas),
            contenido: contenido.into(),
            grupo_id: None,
            grupo: None,
        }
    }

    #[test]
    fn marca_donde_empieza_y_donde_termina_cada_charla() {
        let ahora = DateTime::UNIX_EPOCH + chrono::Duration::hours(1000);
        // Del más nuevo al más viejo, como vienen de la base.
        let mensajes = [
            mensaje(Some("a"), 1, "listo, gracias"),
            mensaje(Some("b"), 2, "dale"),
            mensaje(Some("a"), 3, "hola, tenes un minuto?"),
            mensaje(Some("a"), 30, "nos vemos"),
            mensaje(Some("a"), 31, "buenas, como va?"),
            mensaje(None, 40, "Hola: te escribo por la factura. Saludos."),
        ];
        let marcas = |completos| -> Vec<(bool, bool)> {
            charlas(&mensajes, completos, ahora).iter().map(|m| (m.abre, m.cierra)).collect()
        };
        assert_eq!(
            marcas(false),
            [
                // El último de «a» es de hace una hora: todavía puede seguir.
                (false, false),
                (false, false),
                // Arrancó de nuevo después de más de un día de silencio…
                (true, false),
                // …y con el anterior había terminado la charla de antes.
                (false, true),
                // Sin saber qué hubo antes, el más viejo no cuenta como comienzo.
                (false, false),
                // Lo que no es de ninguna conversación es una charla entera.
                (true, true),
            ]
        );
        // Con todos los mensajes a la vista, el más viejo de cada conversación sí es un comienzo.
        let completos = marcas(true);
        assert_eq!((completos[1], completos[4]), ((true, false), (true, false)));
        // Pasadas las horas, el más nuevo también cerró su charla.
        let despues = charlas(&mensajes, false, ahora + chrono::Duration::hours(HORAS_ENTRE_CHARLAS));
        assert!(despues[0].cierra && despues[1].cierra);
    }

    #[test]
    fn reconoce_el_lugar_dicho_de_varias_formas() {
        assert_eq!(leer_lugar(" E-mail "), "correo");
        assert_eq!(leer_lugar("WhatsApp"), "whatsapp");
        assert_eq!(leer_lugar("Chat"), "claude");
        assert_eq!(leer_lugar("Slack"), "slack");
    }
}
