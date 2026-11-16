//! Los mensajes que escribió el usuario en sus conversaciones con Claude: la
//! primera fuente de la que la memoria aprende cómo escribe (punto 4.11 del diseño).
//!
//! No todo lo que el archivo marca como «del usuario» lo escribió él. La
//! respuesta dada con un botón trae el texto que le puso Claude; a veces
//! pega un pedazo de la respuesta anterior para preguntar por él; el primer
//! mensaje de una conversación abierta por otra lo escribió esa otra; y muy
//! seguido pega lo que le escribió otra persona («mirá lo que me dicen:»).
//! Por eso acá se mira también qué mostró Claude desde el mensaje anterior,
//! solo vale lo que el archivo marca como escrito por una persona, y de cada
//! mensaje queda nada más que lo que es claramente suyo: ante la duda, se
//! corta. Guardar como propio lo que escribió otro es peor que perder un renglón.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::Value;

use super::idioma;
use crate::lector;

/// Un renglón de Claude más corto que esto puede coincidir por casualidad.
const CITA_MINIMA: usize = 30;
/// Un mensaje más largo que esto casi siempre trae algo pegado.
const LARGO_MAXIMO: usize = 2000;
const RENGLONES_MAXIMOS: usize = 40;
/// Una «palabra» más larga que esto es un dato pegado, no algo escrito.
const PALABRA_MAXIMA: usize = 200;
/// Desde este largo se mira que el mensaje sea sobre todo letras (un
/// registro o un pedazo de código pegado no lo es).
const LARGO_PARA_MIRAR_LETRAS: usize = 80;
const LETRAS_MINIMAS: usize = 60;
/// Cuántos renglones de lo que mostró Claude se recuerdan, como mucho.
const MOSTRADO_MAXIMO: usize = 4000;

/// Con qué empieza lo que pone la app cuando el usuario toca un botón suyo.
const DE_LA_APP: &[&str] =
    &["Try again", "Do this task here:", "Cancel the recurring loop", "Continue from where you left off"];

/// Hasta este largo, lo que está antes de «: » puede ser la presentación de algo pegado…
const PRESENTACION_MAXIMA: usize = 60;
/// …si lo que sigue es por lo menos así de largo.
const PEGADO_MINIMO: usize = 40;

/// Palabras (sin acentos) que, antes de «: », anuncian algo que no es del usuario.
const ANUNCIAN: &[&str] = &[
    "esto",
    "este",
    "esta",
    "mira",
    "dice",
    "dijo",
    "dicen",
    "puso",
    "puse",
    "pregunta",
    "preguntan",
    "pregunto",
    "responde",
    "respondio",
    "contesto",
    "reporta",
    "reportan",
    "error",
    "sale",
    "aparece",
    "mensaje",
    "comentario",
    "siguiente",
    "asi",
    "textual",
];

/// Palabras (sin acentos) que, al final de un párrafo, señalan lo que viene pegado después.
const SENALAN: &[&str] = &["esto", "estos", "siguiente", "siguientes"];
/// Desde esta parte de números (por ciento de lo que no es espacio), un párrafo es un dato pegado…
const NUMEROS_DE_UN_DATO: usize = 15;
/// …si tiene por lo menos este largo.
const LARGO_DE_UN_DATO: usize = 40;
/// Con estos renglones «Etiqueta: valor», un párrafo es una ficha pegada.
const RENGLONES_DE_UNA_FICHA: usize = 3;

/// Un renglón de una ficha: «Nombre del banco: …».
static DE_FICHA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Lu}[^:\n]{2,40}: \S").expect("patrón válido"));
/// Comentarios que deja la app dentro de un mensaje (`<!-- reply 1 -->`).
static COMENTARIO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").expect("patrón válido"));
/// Un renglón copiado de una mensajería: «[22/10, 8:16] Nombre: …».
static DE_MENSAJERIA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[\d{1,2}[/.-]\d{1,2}([/.-]\d{2,4})?,?\s+\d{1,2}:\d{2}").expect("patrón válido"));
/// La hora entre corchetes de un mensaje copiado de un chat de trabajo: «Nombre  [12:51 PM]».
static HORA_DE_CHAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\d{1,2}:\d{2}(\s?[AaPp]\.?[Mm]\.?)?\]").expect("patrón válido"));
/// Un renglón que solo separa: «-----».
static SEPARADOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-=_*~\s]{3,}$").expect("patrón válido"));
/// Frases (sin acentos) con las que se presenta lo que escribió otro.
static PRESENTA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b((esto|lo) que me (dice|dijo|dicen|dijeron|pregunta|pregunto|preguntan|preguntaron|reporta|reportan|reportaron|pide|piden|pidieron|manda|mando|mandan|mandaron|escribe|escribio|escribieron|contesta|contesto|contestaron|responde|respondio|respondieron|pasa|paso|pasaron|dejo|dejaron)|me (dice|dicen|dijo|pregunta|preguntan|pregunto|reporta|reportan|pide|piden|mando|mandaron|escribio|escribieron|contesto|respondio|dejaron|pasaron) (esto|este|esta|estos|estas|lo siguiente)|(mira|fijate|lee) (esto|este|esta|lo que))\b",
    )
    .expect("patrón válido")
});

/// Marcas de formato que se pierden al copiar un texto de la pantalla.
const fn es_formato(c: char) -> bool {
    matches!(c, '*' | '`' | '_' | '#' | '>' | '•')
}

/// Un mensaje que escribió el usuario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Propio {
    pub texto: String,
    pub fecha: DateTime<Utc>,
}

/// Lo que salió de leer un archivo desde una posición.
#[derive(Debug, Default)]
pub struct Lectura {
    pub mensajes: Vec<Propio>,
    /// Dónde termina el último mensaje del usuario que se vio. Desde ahí se
    /// sigue la próxima vez: lo que Claude mostró después hace falta para
    /// entender el mensaje que venga.
    pub hasta: u64,
}

/// Un texto sin marcas de formato y con los espacios juntados, y de qué
/// bytes del original sale cada byte suyo.
#[derive(Default)]
struct Plano {
    texto: String,
    desde: Vec<usize>,
    hasta: Vec<usize>,
}

fn aplanar(texto: &str) -> Plano {
    let mut p = Plano::default();
    let mut espacio: Option<(usize, usize)> = None;
    for (i, c) in texto.char_indices() {
        let fin = i + c.len_utf8();
        if es_formato(c) {
            continue;
        }
        if c.is_whitespace() {
            espacio = Some((espacio.map_or(i, |(desde, _)| desde), fin));
            continue;
        }
        if let Some((desde, hasta)) = espacio.take()
            && !p.texto.is_empty()
        {
            p.texto.push(' ');
            p.desde.push(desde);
            p.hasta.push(hasta);
        }
        p.texto.push(c);
        p.desde.extend(std::iter::repeat_n(i, c.len_utf8()));
        p.hasta.extend(std::iter::repeat_n(fin, c.len_utf8()));
    }
    p
}

/// El renglón sin la viñeta o el número con que empieza una lista.
fn sin_vineta(renglon: &str) -> &str {
    let sin_guion = renglon.trim_start_matches(['-', '–', '—', '·']).trim_start();
    let sin_numero = sin_guion.trim_start_matches(|c: char| c.is_ascii_digit());
    if sin_numero.len() < sin_guion.len()
        && let Some(resto) = sin_numero.strip_prefix(['.', ')'])
    {
        return resto.trim_start();
    }
    sin_guion
}

/// Lo que Claude le mostró al usuario desde su mensaje anterior.
#[derive(Default)]
struct Mostrado {
    /// El texto de los botones que le ofreció.
    botones: HashSet<String>,
    /// Sus renglones, aplanados.
    renglones: VecDeque<String>,
}

impl Mostrado {
    fn texto(&mut self, texto: &str) {
        for renglon in texto.lines() {
            let plano = aplanar(renglon).texto;
            let plano = sin_vineta(&plano);
            if plano.chars().count() < CITA_MINIMA {
                continue;
            }
            if self.renglones.len() == MOSTRADO_MAXIMO {
                self.renglones.pop_front();
            }
            self.renglones.push_back(plano.to_owned());
        }
    }

    fn boton(&mut self, etiqueta: &str) {
        self.botones.insert(etiqueta.trim().to_owned());
    }
}

/// Una lista que puede venir como lista o como texto JSON.
fn lista(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(valores) => valores.clone(),
        Value::String(texto) => serde_json::from_str(texto).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Anota lo que un renglón de Claude le deja a la vista al usuario: su texto,
/// y el resumen y los botones del cierre del turno o de una pregunta.
fn anotar(v: &Value, mostrado: &mut Mostrado) {
    let Some(bloques) = v["message"]["content"].as_array() else { return };
    for bloque in bloques {
        if bloque["type"] == "text" {
            if let Some(texto) = bloque["text"].as_str() {
                mostrado.texto(texto);
            }
            continue;
        }
        if bloque["type"] != "tool_use" {
            continue;
        }
        let entrada = &bloque["input"];
        let nombre = bloque["name"].as_str().unwrap_or_default();
        if nombre.ends_with("wrap_up") {
            for clave in ["recap", "ask"] {
                if let Some(texto) = entrada[clave].as_str() {
                    mostrado.texto(texto);
                }
            }
            for accion in lista(&entrada["actions"]) {
                if let Some(etiqueta) = accion["label"].as_str() {
                    mostrado.boton(etiqueta);
                }
            }
        } else if nombre == "AskUserQuestion" {
            for pregunta in lista(&entrada["questions"]) {
                if let Some(texto) = pregunta["question"].as_str() {
                    mostrado.texto(texto);
                }
                for opcion in lista(&pregunta["options"]) {
                    if let Some(etiqueta) = opcion["label"].as_str() {
                        mostrado.boton(etiqueta);
                    }
                    if let Some(texto) = opcion["description"].as_str() {
                        mostrado.texto(texto);
                    }
                }
            }
        }
    }
}

/// Saca de un mensaje lo que el usuario copió de lo que le mostró Claude.
fn quitar_citas(texto: &str, mostrado: &Mostrado) -> String {
    if mostrado.renglones.is_empty() {
        return texto.to_owned();
    }
    // Renglones enteros del mensaje que son un pedazo de lo mostrado.
    let todo = mostrado.renglones.iter().map(String::as_str).collect::<Vec<_>>().join("\n");
    let mut queda = String::new();
    for renglon in texto.split_inclusive('\n') {
        let plano = aplanar(renglon).texto;
        let plano = sin_vineta(&plano);
        if plano.chars().count() < CITA_MINIMA || !todo.contains(plano) {
            queda.push_str(renglon);
        }
    }
    // Renglones de lo mostrado metidos en medio de lo que escribió.
    let plano = aplanar(&queda);
    let mut tapados: Vec<(usize, usize)> = Vec::new();
    for cita in &mostrado.renglones {
        let mut desde = 0;
        while let Some(i) = plano.texto[desde..].find(cita.as_str()) {
            let (inicio, fin) = (desde + i, desde + i + cita.len());
            tapados.push((plano.desde[inicio], plano.hasta[fin - 1]));
            desde = fin;
        }
    }
    if tapados.is_empty() {
        return queda;
    }
    tapados.sort_unstable();
    let mut salida = String::new();
    let mut posicion = 0;
    for (inicio, fin) in tapados {
        if inicio > posicion {
            agregar(&mut salida, &queda[posicion..inicio]);
        }
        posicion = posicion.max(fin);
    }
    agregar(&mut salida, &queda[posicion..]);
    salida
}

/// Suma un pedazo sin dejar restos donde se sacó una cita: ni dos espacios
/// seguidos, ni un renglón que empiece con los dos puntos o la coma que
/// venían después de lo citado.
fn agregar(salida: &mut String, pedazo: &str) {
    let al_empezar_un_renglon = salida.is_empty() || salida.ends_with('\n');
    let pedazo = if al_empezar_un_renglon {
        pedazo.trim_start_matches([' ', '\t', ':', ',', ';'])
    } else if salida.ends_with(char::is_whitespace) {
        pedazo.trim_start_matches([' ', '\t'])
    } else {
        pedazo
    };
    salida.push_str(pedazo);
}

/// Sin espacios en las puntas ni renglones vacíos de más.
fn ordenar(texto: &str) -> String {
    let mut salida = String::new();
    let mut vacios = 0;
    for renglon in texto.trim().lines() {
        let renglon = renglon.trim_end();
        vacios = if renglon.is_empty() { vacios + 1 } else { 0 };
        if vacios > 1 {
            continue;
        }
        if !salida.is_empty() {
            salida.push('\n');
        }
        salida.push_str(renglon);
    }
    salida
}

/// Si un renglón (o un párrafo, por su primer renglón) parece copiado de otro
/// lado: de una mensajería, de un chat de trabajo, una cita, una lista con las
/// viñetas de un texto ya armado, o algo que alguien le escribió al usuario
/// (`nombre`).
fn parece_copiado(texto: &str, nombre: &str) -> bool {
    let primero = texto.lines().next().unwrap_or_default().trim();
    if DE_MENSAJERIA.is_match(primero) || HORA_DE_CHAT.is_match(primero) {
        return true;
    }
    if primero.starts_with(['>', '“', '"', '@']) || primero.starts_with("* ") || primero.starts_with("• ") {
        return true;
    }
    let nombre = idioma::plano(nombre.trim());
    if nombre.chars().count() < 3 {
        return false;
    }
    let empieza = idioma::plano(primero);
    let saludado = ["hola ", "buenas ", "che ", "hi ", "hey ", "hello "]
        .iter()
        .any(|saludo| empieza.strip_prefix(saludo).is_some_and(|resto| resto.starts_with(&nombre)));
    let nombrado =
        empieza.strip_prefix(&nombre).is_some_and(|resto| resto.starts_with([',', ':', '!']) || resto.is_empty());
    saludado || nombrado || empieza.contains(&format!("@{nombre}"))
}

/// Si un párrafo presenta algo que viene después y no es del usuario:
/// termina en dos puntos o señalando («¿son estos?»), o lo dice («mirá lo
/// que me dicen»).
fn presenta(parrafo: &str) -> bool {
    let fin = parrafo.trim_end().trim_end_matches('?');
    let ultima = idioma::palabras(fin).pop().unwrap_or_default();
    fin.ends_with(':') || SENALAN.contains(&ultima.as_str()) || PRESENTA.is_match(&idioma::plano(parrafo))
}

/// Si un párrafo es un dato pegado y no algo escrito: una tabla o un listado
/// lleno de números, o una ficha de renglones «Etiqueta: valor».
fn parece_dato(parrafo: &str) -> bool {
    let visibles = parrafo.chars().filter(|c| !c.is_whitespace()).count();
    let numeros = parrafo.chars().filter(char::is_ascii_digit).count();
    if visibles >= LARGO_DE_UN_DATO && numeros * 100 >= visibles * NUMEROS_DE_UN_DATO {
        return true;
    }
    parrafo.lines().filter(|renglon| DE_FICHA.is_match(renglon.trim_start())).count() >= RENGLONES_DE_UNA_FICHA
}

/// Si el renglón presenta algo y lo trae pegado ahí mismo («mira: Could not
/// load…»), dónde termina la presentación.
fn fin_de_la_presentacion(renglon: &str) -> Option<usize> {
    let corte = renglon.find(": ")?;
    let (antes, despues) = (&renglon[..corte], &renglon[corte + 2..]);
    let anuncia = idioma::palabras(antes).iter().any(|palabra| ANUNCIAN.contains(&palabra.as_str()));
    (antes.chars().count() <= PRESENTACION_MAXIMA && despues.trim().chars().count() >= PEGADO_MINIMO && anuncia)
        .then_some(corte + 1)
}

/// De un párrafo, lo que es del usuario, y si ahí se corta el mensaje porque
/// lo que sigue es pegado.
fn propio_del_parrafo(parrafo: &str, nombre: &str) -> (String, bool) {
    let mut queda: Vec<&str> = Vec::new();
    let renglones: Vec<&str> = parrafo.lines().collect();
    for (i, renglon) in renglones.iter().enumerate() {
        if i > 0 && parece_copiado(renglon, nombre) {
            return (queda.join("\n"), true);
        }
        if let Some(fin) = fin_de_la_presentacion(renglon) {
            queda.push(&renglon[..fin]);
            return (queda.join("\n"), true);
        }
        queda.push(renglon);
        if i + 1 < renglones.len() && renglon.trim_end().ends_with(':') {
            return (queda.join("\n"), true);
        }
    }
    (queda.join("\n"), false)
}

/// Lo que en un mensaje es claramente del usuario: se queda con los párrafos
/// del principio y corta en el primero que parece copiado, que cambia de
/// idioma o que viene después de una presentación.
fn solo_lo_propio(texto: &str, nombre: &str) -> String {
    let texto = COMENTARIO.replace_all(texto, "");
    let texto = texto.trim();
    // Un bloque entre etiquetas es una orden armada por la app.
    if texto.starts_with('<') && texto[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return String::new();
    }
    let mut propios: Vec<String> = Vec::new();
    let mut del_principio = idioma::SIN_IDIOMA;
    for parrafo in texto.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if SEPARADOR.is_match(parrafo) || parece_copiado(parrafo, nombre) || parece_dato(parrafo) {
            break;
        }
        let de_este = idioma::de(parrafo);
        if propios.is_empty() {
            del_principio = de_este;
        } else if de_este != idioma::SIN_IDIOMA && del_principio != idioma::SIN_IDIOMA && de_este != del_principio {
            break;
        }
        let (propio, hasta_aca) = propio_del_parrafo(parrafo, nombre);
        let corta = hasta_aca || presenta(&propio);
        if !propio.trim().is_empty() {
            propios.push(propio);
        }
        if corta {
            break;
        }
    }
    propios.join("\n\n")
}

/// Si parece escrito a mano: ni larguísimo, ni un pedazo de código, ni un
/// registro o un dato pegados.
fn parece_escrito(texto: &str) -> bool {
    let largo = texto.chars().count();
    if largo == 0 || largo > LARGO_MAXIMO || texto.contains("```") || texto.lines().count() > RENGLONES_MAXIMOS {
        return false;
    }
    if texto.split_whitespace().any(|palabra| palabra.chars().count() > PALABRA_MAXIMA) {
        return false;
    }
    if largo < LARGO_PARA_MIRAR_LETRAS {
        return true;
    }
    let visibles = texto.chars().filter(|c| !c.is_whitespace()).count();
    let letras = texto.chars().filter(|c| c.is_alphabetic()).count();
    letras * 100 >= visibles * LETRAS_MINIMAS
}

/// Si el archivo dice que el renglón lo escribió una persona. Los avisos de
/// tareas, los mensajes de otras conversaciones y los de los agregados de la
/// app entran como «user» con otro origen, o sin ninguno.
fn es_de_una_persona(v: &Value) -> bool {
    let marcado = |clave: &str| v[clave].as_bool() == Some(true);
    if marcado("isMeta") || marcado("isCompactSummary") {
        return false;
    }
    v["origin"].as_str().or_else(|| v["origin"]["kind"].as_str()) == Some("human")
}

/// Lo que escribió el usuario (`nombre`) en un renglón suyo, si escribió algo.
fn mensaje(v: &Value, mostrado: &Mostrado, nombre: &str) -> Option<Propio> {
    let partes = lector::textos(&v["message"]["content"])?;
    let texto = partes.iter().filter_map(|t| lector::limpiar(t)).collect::<Vec<_>>().join("\n\n");
    let texto = texto.trim();
    if texto.is_empty() || mostrado.botones.contains(texto) || DE_LA_APP.iter().any(|d| texto.starts_with(d)) {
        return None;
    }
    let propio = ordenar(&solo_lo_propio(&quitar_citas(texto, mostrado), nombre));
    if !parece_escrito(&propio) {
        return None;
    }
    let fecha = v["timestamp"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok())?.to_utc();
    Some(Propio { texto: propio, fecha })
}

/// Lee el archivo de una conversación desde `desde` (el final de un mensaje
/// del usuario, o el principio) y devuelve lo que él escribió después.
/// `nombre` es cómo se llama: sirve para reconocer lo que otros le escribieron
/// a él. Un renglón a medio escribir queda para la próxima.
pub fn leer(archivo: &Path, desde: u64, nombre: &str) -> std::io::Result<Lectura> {
    let mut abierto = File::open(archivo)?;
    abierto.seek(SeekFrom::Start(desde))?;
    let mut entrada = BufReader::with_capacity(1 << 20, abierto);
    let mut lectura = Lectura { mensajes: Vec::new(), hasta: desde };
    let mut mostrado = Mostrado::default();
    let mut posicion = desde;
    let mut renglon = Vec::new();
    loop {
        renglon.clear();
        let leidos = entrada.read_until(b'\n', &mut renglon)?;
        if leidos == 0 || renglon.last() != Some(&b'\n') {
            break;
        }
        posicion += u64::try_from(leidos).unwrap_or(u64::MAX);
        let del_usuario = lector::contiene(&renglon, br#""type":"user""#);
        if !del_usuario && !lector::contiene(&renglon, br#""type":"assistant""#) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&renglon) else { continue };
        if v["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        if v["type"] == "assistant" {
            anotar(&v, &mut mostrado);
        } else if v["type"] == "user" && es_de_una_persona(&v) {
            lectura.mensajes.extend(mensaje(&v, &mostrado, nombre));
            mostrado = Mostrado::default();
            lectura.hasta = posicion;
        }
    }
    Ok(lectura)
}

#[cfg(test)]
mod pruebas {
    use std::io::Write as _;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn del_usuario(texto: &str) -> Value {
        json!({
            "type": "user", "origin": { "kind": "human" }, "timestamp": "2026-11-12T18:27:01.745Z",
            "message": { "role": "user", "content": texto },
        })
    }

    fn de_claude(texto: &str, cierre: Option<Value>) -> Value {
        let mut bloques = vec![json!({ "type": "text", "text": texto })];
        if let Some(entrada) = cierre {
            bloques.push(json!({ "type": "tool_use", "name": "mcp__ccd_turn__wrap_up", "input": entrada }));
        }
        json!({ "type": "assistant", "message": { "model": "claude-opus-5-5", "content": bloques } })
    }

    fn archivo(nombre: &str, renglones: &[Value]) -> PathBuf {
        let ruta = std::env::temp_dir().join(format!("memoria-propios-{nombre}-{}.jsonl", std::process::id()));
        let mut f = File::create(&ruta).unwrap();
        for renglon in renglones {
            writeln!(f, "{renglon}").unwrap();
        }
        ruta
    }

    fn textos(lectura: &Lectura) -> Vec<&str> {
        lectura.mensajes.iter().map(|m| m.texto.as_str()).collect()
    }

    #[test]
    fn solo_vale_lo_que_escribio_una_persona() {
        let ruta = archivo(
            "origen",
            &[
                // El encargo con que otra conversación abrió esta: no trae origen.
                json!({ "type": "user", "timestamp": "2026-11-12T18:00:00Z", "message": { "content": "Background: armá el servicio de punta a punta." } }),
                json!({ "type": "user", "origin": { "kind": "task-notification" }, "timestamp": "2026-11-12T18:00:01Z",
                        "message": { "content": "<task-notification>terminó</task-notification>" } }),
                json!({ "type": "user", "origin": { "kind": "plugin", "name": "memoria" }, "timestamp": "2026-11-12T18:00:02Z",
                        "message": { "content": "The memoria plugin sent a message: retomá lo que estabas haciendo." } }),
                json!({ "type": "user", "isMeta": true, "origin": { "kind": "human" }, "timestamp": "2026-11-12T18:00:03Z",
                        "message": { "content": "aviso de la app" } }),
                json!({ "type": "user", "isSidechain": true, "origin": { "kind": "human" }, "timestamp": "2026-11-12T18:00:04Z",
                        "message": { "content": "esto se lo dijo Claude a un subagente" } }),
                json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "content": "salida de una herramienta" }] } }),
                del_usuario(
                    "<system-reminder>recordatorio</system-reminder>\nterminaste? falta algo o ya se puede usar?",
                ),
                del_usuario("<bash-input>git push</bash-input><bash-stdout>ok</bash-stdout>"),
                del_usuario("Try again"),
            ],
        );
        let lectura = leer(&ruta, 0, "Ana").unwrap();
        std::fs::remove_file(&ruta).unwrap();
        assert_eq!(textos(&lectura), vec!["terminaste? falta algo o ya se puede usar?"]);
        assert_eq!(lectura.mensajes[0].fecha.to_rfc3339(), "2026-11-12T18:27:01.745+00:00");
    }

    #[test]
    fn una_respuesta_dada_con_un_boton_no_la_escribio_el_usuario() {
        let cierre = json!({ "recap": "Quedó instalada.", "ask": "¿La lanzo otra vez?",
                             "actions": [{ "label": "Sí, otra vez" }, { "label": "No, dejala así" }] });
        // Los botones también pueden llegar como texto JSON.
        let como_texto = json!({ "recap": "Listo.", "actions": "[{\"label\": \"Sí, guardalo\"}]" });
        let ruta = archivo(
            "botones",
            &[
                de_claude("La mejora quedó descartada por una prueba.", Some(cierre)),
                del_usuario("Sí, otra vez"),
                de_claude("La lancé de nuevo.", Some(como_texto)),
                del_usuario("Sí, guardalo"),
                // Sin botones a la vista, el mismo texto sí lo escribió él.
                de_claude("Guardado.", None),
                del_usuario("Sí, otra vez"),
            ],
        );
        let lectura = leer(&ruta, 0, "Ana").unwrap();
        std::fs::remove_file(&ruta).unwrap();
        assert_eq!(textos(&lectura), vec!["Sí, otra vez"]);
    }

    #[test]
    fn saca_lo_que_el_usuario_copio_de_la_respuesta_anterior() {
        let respuesta = "Hice tres cosas:\n\n- **Versión 2.1 instalada:** el servicio nuevo corre de fondo, sin configuración.\n- Las pruebas pasan y el tablero muestra 27 de 27.\n\nFalta verlo con un pedido de verdad.";
        let cierre = json!({ "recap": "Versión 2.1 instalada: el servicio corre de fondo. Falta verlo con un pedido de verdad.",
                             "ask": "¿Querés que lo pruebe ahora con el pedido que falló ayer?" });
        let ruta = archivo(
            "citas",
            &[
                de_claude(respuesta, Some(cierre)),
                // Pegado en el mismo renglón que la pregunta, como sale del recuadro.
                del_usuario(
                    "que quiere decir esto?  Versión 2.1 instalada: el servicio corre de fondo. Falta verlo con un pedido de verdad.\n\n¿Querés que lo pruebe ahora con el pedido que falló ayer?",
                ),
                de_claude(respuesta, None),
                // Pegado como renglones aparte y sin el formato: uno entero y un pedazo de otro.
                del_usuario(
                    "no entiendo esta parte\nVersión 2.1 instalada: el servicio nuevo corre de fondo, sin configuración.\nni esta\nel servicio nuevo corre de fondo, sin configuración\nque significa?",
                ),
                // Lo que mostró antes del mensaje anterior ya no cuenta.
                de_claude("Te explico.", None),
                del_usuario("ok. Las pruebas pasan y el tablero muestra 27 de 27. eso lo escribi yo"),
            ],
        );
        let lectura = leer(&ruta, 0, "Ana").unwrap();
        std::fs::remove_file(&ruta).unwrap();
        assert_eq!(
            textos(&lectura),
            vec![
                "que quiere decir esto?",
                "no entiendo esta parte\nni esta\nque significa?",
                "ok. Las pruebas pasan y el tablero muestra 27 de 27. eso lo escribi yo",
            ]
        );
    }

    #[test]
    fn de_un_mensaje_queda_solo_lo_que_escribio_el_usuario() {
        let propio = |texto: &str| solo_lo_propio(texto, "Ana");
        // Presenta lo que le escribió otro: queda la presentación.
        assert_eq!(
            propio(
                "podes confirmarme esto que me pregunta Luis?\n\nAna, consulta: mañana abre la oficina o es feriado?"
            ),
            "podes confirmarme esto que me pregunta Luis?"
        );
        assert_eq!(
            propio(
                "Mira lo que me reportan:\n\n[22/10, 8:16] Pedro: Hablé con la oficina y está todo bien.\n[22/10, 8:17] Pedro: Arrancan el lunes."
            ),
            "Mira lo que me reportan:"
        );
        assert_eq!(
            propio(
                "ahora con estas:\n\nMarta  [12:51 PM]\n@Ana sumé una tarea más al tablero\n\nSi tenes algo listo pasalo"
            ),
            "ahora con estas:"
        );
        // Pegado en el mismo renglón, o en el mismo párrafo.
        assert_eq!(
            propio(
                "mira: Could not open settings.json: the file is missing or it is not valid JSON\n\nCuando abro la página me dice esto"
            ),
            "mira:"
        );
        assert_eq!(
            propio("podes ver esto?\nSofi  [1:46 PM]\nHey! I'm sharing the notes from today."),
            "podes ver esto?"
        );
        assert_eq!(propio("fijate\nAna, cuando puedas pasame el link\ngracias"), "fijate");
        // Cambia de idioma en el medio: lo de después no es suyo.
        assert_eq!(
            propio(
                "necesito contestar algo de esto hoy\n\nhey team, can you confirm that the two accounts are set on your side?"
            ),
            "necesito contestar algo de esto hoy"
        );
        // Una lista con viñetas de texto ya armado, un separador, un bloque de la app.
        assert_eq!(
            propio("Decidí vos\n\n* Cómo se entrega la primera parte: por correo\n* Si se acepta el diseño como está"),
            "Decidí vos"
        );
        assert_eq!(
            propio("Me dejaron una nota en el tablero\n\n-------------\n\nNo la contestes todavía"),
            "Me dejaron una nota en el tablero"
        );
        assert_eq!(propio("<create-pr-command>\n## Overview\n\nCreate a pull request.\n</create-pr-command>"), "");
        assert_eq!(propio("[16/11/26, 10:15:02] Pedro: hola, ¿cómo va?"), "");
        assert_eq!(
            propio("<!-- reply 1 -->\n\nNo entiendo, eso se lo tengo que pedir yo?"),
            "No entiendo, eso se lo tengo que pedir yo?"
        );

        // Lo que es todo suyo queda entero, aunque tenga varios párrafos, listas o dos puntos.
        for suyo in [
            "Entendi mal, como es la prueba?\n\nComo deberia ser? tengo un tablero, un chat y una planilla",
            "vamos por partes, me preocupa hacer tantas cosas a mano.\n\n1- Base: en un contenedor esta bien\n2- Web: igual que el 1\n3- Servicio: no hay manera de probarlo sin subirlo?",
            "Una cosa: no quiero que quede ningun nombre propio en el repositorio",
            "No me gusta el nombre. Ponele otro que sea neutro.\n\ny subilo cuando lo tengas",
        ] {
            assert_eq!(propio(suyo), suyo);
        }
        // Señala lo que viene, o lo que viene es un dato pegado: una tabla, una ficha.
        assert_eq!(
            propio(
                "los datos de la cuenta cambiaron. podes ver si son estos?\n\nBanco: Uno\nSucursal: Centro\nTitular: Ana"
            ),
            "los datos de la cuenta cambiaron. podes ver si son estos?"
        );
        assert_eq!(
            propio(
                "revisá los turnos\n\n26 2/7/2026 10:25 PEREZ, ANA CONSULTA 55.400,00\n18 2/7/2026 09:30 GOMEZ, LUIS CONTROL 31.000,00"
            ),
            "revisá los turnos"
        );
        assert_eq!(
            propio("te paso todo\n\nBanco: Uno\nSucursal: Centro\nTitular: Ana Pérez\n\ngracias"),
            "te paso todo"
        );
        // Ante la duda se corta, aunque se pierda algo suyo.
        assert_eq!(propio("ojo con esto: no lo subas todavia porque falta revisar las pruebas"), "ojo con esto:");
        // Sin saber cómo se llama, no se puede reconocer lo que le escriben a él.
        assert_eq!(solo_lo_propio("fijate\nAna, pasame el link", "Usuario"), "fijate\nAna, pasame el link");
    }

    #[test]
    fn deja_afuera_lo_pegado_y_lo_que_no_es_texto() {
        assert!(parece_escrito("ok!!"));
        assert!(parece_escrito("👍"));
        assert!(parece_escrito("mirá https://example.com/una/direccion/larga y decime"));
        assert!(!parece_escrito(""));
        assert!(!parece_escrito(&"palabra ".repeat(400)));
        assert!(!parece_escrito("probá esto:\n```\ncargo test\n```"));
        assert!(!parece_escrito(&format!("la clave es {}", "x".repeat(250))));
        let registro = "2026-11-16T14:21:59.415941Z INFO guardian::instalar: mejora=1 [0x7f] {\"a\": 1, \"b\": [2, 3]} => 27/28 (96.43%) #### 12:00:01 -> 12:00:09";
        assert!(!parece_escrito(registro));
        assert!(!parece_escrito(&"un renglón\n".repeat(41)));
    }

    #[test]
    fn sigue_desde_el_ultimo_mensaje_del_usuario() {
        let cierre = json!({ "recap": "Hecho.", "actions": [{ "label": "Sí, subila" }] });
        let primero = [del_usuario("armá la versión nueva"), de_claude("Armada.", Some(cierre))];
        let ruta = archivo("seguir", &primero);
        let lectura = leer(&ruta, 0, "Ana").unwrap();
        assert_eq!(textos(&lectura), vec!["armá la versión nueva"]);
        // Quedó parado después del mensaje del usuario, no al final del archivo:
        // así, cuando llegue la respuesta, se sabe qué botones tenía a la vista.
        let tamano = std::fs::metadata(&ruta).unwrap().len();
        assert!(lectura.hasta > 0 && lectura.hasta < tamano);

        let mut f = std::fs::OpenOptions::new().append(true).open(&ruta).unwrap();
        writeln!(f, "{}", del_usuario("Sí, subila")).unwrap();
        writeln!(f, "{}", del_usuario("y avisame cuando termine")).unwrap();
        write!(f, "{{\"type\":\"user\",\"origin\":{{\"kind\":\"human\"}},\"mess").unwrap();
        drop(f);
        let segunda = leer(&ruta, lectura.hasta, "Ana").unwrap();
        std::fs::remove_file(&ruta).unwrap();
        assert_eq!(textos(&segunda), vec!["y avisame cuando termine"]);
        assert!(segunda.hasta > lectura.hasta);
    }

    #[test]
    fn aplana_y_recuerda_de_donde_salio_cada_letra() {
        let p = aplanar("  **Hola**,   `mundo`\n ñandú ");
        assert_eq!(p.texto, "Hola, mundo ñandú");
        assert_eq!(p.desde.len(), p.texto.len());
        // La «ñ» ocupa dos bytes y los dos apuntan a la misma letra.
        let i = p.texto.find('ñ').unwrap();
        assert_eq!(p.desde[i], p.desde[i + 1]);
        assert_eq!(p.hasta[i] - p.desde[i], 2);
        assert_eq!(sin_vineta("- un renglón"), "un renglón");
        assert_eq!(sin_vineta("12. otro renglón"), "otro renglón");
        assert_eq!(sin_vineta("2026 fue un año largo"), "2026 fue un año largo");
    }
}
