//! Estilo del usuario (punto 4.11 del diseño): la memoria junta lo que él
//! escribe, mide cómo lo escribe y se lo cuenta a Claude cuando tiene que
//! redactar algo que va a salir con su nombre.
//!
//! Tres partes: de dónde salen los mensajes ([`propios`], por ahora sus
//! conversaciones con Claude), qué se mide en ellos ([`numeros`]) y lo que
//! responde la herramienta `memoria_estilo` ([`consultar`]). Todo queda en la
//! base de esta máquina y no hay ningún modelo de por medio: son cuentas.

pub mod fuentes;
pub mod idioma;
pub mod lexico;
pub mod numeros;
pub mod propios;
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
/// Con menos mensajes que estos, el perfil es apenas una pista.
const POCOS: usize = 30;

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
    /// A quién se le va a escribir.
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
    para: Option<String>,
    fecha: DateTime<Utc>,
    contenido: String,
    grupo_id: Option<i64>,
    /// El nombre del grupo de su conversación, si tiene.
    grupo: Option<String>,
}

type Fila =
    (String, String, String, Option<String>, DateTime<Utc>, String, Option<i64>, Option<String>, Option<String>);

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
/// idioma; con `con_cortos`, también los que son muy cortos para tener idioma.
async fn mensajes(pool: &SqlitePool, lugar: Option<&str>, idioma: &str, con_cortos: bool) -> Resultado<Vec<Mensaje>> {
    let filas: Vec<Fila> = sqlx::query_as(
        "select m.fuente, m.lugar, m.idioma, m.para, m.fecha, m.contenido, c.grupo_id, g.empresa, g.proyecto
         from mensajes_propios m
         left join conversaciones c on m.fuente = 'claude' and c.id = m.conversacion
         left join grupos g on g.id = c.grupo_id
         where (?1 is null or m.lugar = ?1) and (m.idioma = ?2 or (?3 and m.idioma = ''))
         order by m.fecha desc, m.id desc limit ?4",
    )
    .bind(lugar)
    .bind(idioma)
    .bind(con_cortos)
    .bind(MAXIMO_PARA_MEDIR)
    .fetch_all(pool)
    .await?;
    Ok(filas
        .into_iter()
        .map(|(fuente, lugar, idioma, para, fecha, contenido, grupo_id, empresa, proyecto)| Mensaje {
            lugar,
            idioma,
            dirigido: fuente != FUENTE_CLAUDE,
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

/// Si un mensaje se le puede mostrar a quien pregunta. Lo que el usuario le
/// mandó a alguien (por una mensajería, por correo) es de esa charla: se
/// muestra solo cuando se pregunta cómo le escribe a esa persona o a ese
/// grupo, y se sabe que era para ella. Lo que le escribió a Claude vale siempre.
fn se_puede_mostrar(m: &Mensaje, para: Option<&str>) -> bool {
    if !m.dirigido {
        return true;
    }
    match (&m.para, para) {
        (Some(destinatario), Some(pedido)) => idioma::plano(destinatario).contains(pedido),
        _ => false,
    }
}

/// Elige los mensajes que mejor sirven de ejemplo, entre los que se pueden
/// mostrar. Los del grupo de la conversación que pregunta van siempre antes
/// que los de otros, que solo completan si faltan. Dentro de cada parte:
/// primero lo que habla de lo mismo, y entre iguales lo más nuevo.
fn elegir<'a>(mensajes: &'a [Mensaje], p: &Pedido<'_>, idioma: &str, cuantos: usize) -> Vec<&'a Mensaje> {
    let para = p.para.map(|t| idioma::plano(t.trim())).filter(|t| !t.is_empty());
    let sobre = p.sobre.map(texto::Consulta::de);
    let mut candidatos: Vec<(bool, usize, usize, &Mensaje)> = mensajes
        .iter()
        .enumerate()
        .filter(|(_, m)| m.idioma == idioma && sirve_de_ejemplo(&m.contenido))
        .filter(|(_, m)| se_puede_mostrar(m, para.as_deref()))
        .map(|(orden, m)| {
            let mut puntos = 0;
            if let Some(sobre) = &sobre {
                let indexado = texto::para_indexar(&m.contenido);
                puntos += sobre.coinciden(&texto::conjunto(&indexado));
            }
            let de_otro_grupo = p.grupo.is_some_and(|grupo| m.grupo_id != Some(grupo.id));
            (de_otro_grupo, puntos, orden, m)
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

/// Lo que responde `memoria_estilo`: cómo escribe el usuario en un lugar y
/// un idioma, y mensajes suyos de ejemplo.
pub async fn consultar(pool: &SqlitePool, p: &Pedido<'_>) -> Resultado<String> {
    let cuentas = cuentas(pool).await?;
    if cuentas.is_empty() {
        return Ok(SIN_MENSAJES.to_owned());
    }
    let pedido = p.lugar.map(leer_lugar).filter(|l| !l.is_empty());
    let lugar = pedido.as_deref().filter(|l| cuentas.iter().any(|(conocido, _, _)| conocido == l));
    let principal = idioma_principal(&cuentas, lugar);
    let idioma = match p.idioma.map(str::trim).filter(|i| !i.is_empty()) {
        None => principal.clone(),
        Some(dicho) => match idioma::leer(dicho) {
            Some(codigo) => codigo.to_owned(),
            None => return uso(format!("No conozco el idioma «{dicho}»: pasá «es», «en» o «pt».")),
        },
    };
    let mensajes = mensajes(pool, lugar, &idioma, idioma == principal).await?;

    let donde = lugar.map_or_else(|| "todos los lugares juntos".to_owned(), str::to_owned);
    let en = if idioma.is_empty() { String::new() } else { format!(" · {}", idioma::nombre(&idioma)) };
    let mut salida = String::new();
    if let (Some(pedido), None) = (&pedido, lugar) {
        let _ = writeln!(
            salida,
            "No tengo mensajes suyos de «{pedido}»: lo de abajo sale de {}. Sirve para el vocabulario y el tono; \
             las tildes, los signos y los saludos pueden ser otros ahí, así que ante la duda escribí con cuidado.\n",
            lugares(&cuentas)
        );
    }
    if mensajes.len() < MINIMO_PARA_PERFIL {
        let cuantos = match mensajes.len() {
            0 => "No tengo mensajes suyos".to_owned(),
            1 => "Tengo un solo mensaje suyo".to_owned(),
            n => format!("Tengo solo {n} mensajes suyos"),
        };
        let no_alcanza = if mensajes.is_empty() { "" } else { ": no alcanza para saber cómo escribe ahí" };
        let _ = write!(
            salida,
            "{cuantos} en {} ({donde}){no_alcanza}. Tengo de: {}.",
            idioma::nombre(&idioma),
            lugares(&cuentas)
        );
        return Ok(salida);
    }
    let textos: Vec<&str> = mensajes.iter().map(|m| m.contenido.as_str()).collect();
    let numeros = numeros::medir(&textos, &idioma);
    let (primero, ultimo) = (mensajes.last().map(|m| m.fecha), mensajes.first().map(|m| m.fecha));
    let _ = writeln!(
        salida,
        "Así escribe el usuario en {donde}{en} ({} mensajes suyos, del {} al {}):",
        mensajes.len(),
        primero.map(dia).unwrap_or_default(),
        ultimo.map(dia).unwrap_or_default()
    );
    for linea in numeros::describir(&numeros, &idioma) {
        let _ = writeln!(salida, "- {linea}");
    }
    if mensajes.len() < POCOS {
        let _ = writeln!(salida, "Son pocos mensajes: tomalo como una pista, no como una regla.");
    }

    let cuantos = p.ejemplos.unwrap_or(EJEMPLOS_POR_DEFECTO).min(EJEMPLOS_MAXIMOS);
    let elegidos = elegir(&mensajes, p, &idioma, cuantos);
    if !elegidos.is_empty() {
        let _ = writeln!(salida, "\nMensajes suyos, para tomar el tono (no copies lo que dicen):");
        for (i, m) in elegidos.iter().enumerate() {
            let _ = writeln!(salida, "{}", ejemplo(m, i + 1, p));
        }
    } else if mensajes.iter().any(|m| m.dirigido) {
        let _ = writeln!(
            salida,
            "\nNo muestro mensajes suyos de ejemplo: lo que le mandó a alguien se ve solo si decís a quién le va a \
             escribir (`para`: el nombre de la persona o del grupo) y ya le escribió antes."
        );
    }
    let _ = write!(
        salida,
        "\nEscribí como él: el mismo largo, las mismas mayúsculas, tildes y signos, y sus expresiones. Vale para lo \
         que va a salir con su nombre; con él seguí hablando como siempre. Tengo mensajes suyos de: {}.",
        lugares(&cuentas)
    );
    Ok(salida)
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
    fn reconoce_el_lugar_dicho_de_varias_formas() {
        assert_eq!(leer_lugar(" E-mail "), "correo");
        assert_eq!(leer_lugar("WhatsApp"), "whatsapp");
        assert_eq!(leer_lugar("Chat"), "claude");
        assert_eq!(leer_lugar("Slack"), "slack");
    }
}
