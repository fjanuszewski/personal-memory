//! Cómo escribe el usuario, en números: lo que el programa mide en sus
//! mensajes (largo, mayúsculas, signos, tildes, trato, expresiones que
//! repite) y cómo se le cuenta a Claude. No hay modelo: son cuentas, así que
//! dan siempre lo mismo y se pueden probar.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::Serialize;

use super::idioma;
use super::lexico::{
    self, CONTRACCIONES, DESPEDIDAS, LARGO_DE_TERMINACION, MULETILLAS, PRESTADAS, SALUDOS, TERMINACIONES_CON_TILDE,
    TILDES, TUTEO, VOSEO,
};

/// Con menos casos que estos no se saca una conclusión.
const BASE_MINIMA: usize = 5;
/// Cuántas palabras o expresiones se nombran en cada lista.
const NOMBRADAS: usize = 6;
/// Una expresión entra en una lista si aparece al menos estas veces.
const REPETIDA: usize = 2;
/// Las últimas palabras de un mensaje, donde se busca la despedida.
const COLA: usize = 4;
/// Desde qué parte de los mensajes (por ciento) vale la pena nombrar los
/// emojis: los de alguien que casi no los usa no dicen nada de cómo escribe.
const EMOJIS_HABITUALES: usize = 10;

/// Contracciones del inglés que se reconocen aunque falte el apóstrofo.
const SIN_APOSTROFO: &[&str] = &["dont", "isnt", "doesnt", "didnt", "im", "ive", "youre", "thats"];

/// Primeras palabras que no dicen nada de cómo arranca alguien.
const ARRANQUES_SIN_INTERES: &[&str] = &[
    "el", "la", "los", "las", "un", "una", "de", "en", "a", "the", "an", "in", "on", "o", "os", "as", "que", "no",
    "si", "es", "se", "lo", "me", "te", "por", "para", "con", "i", "it", "is", "to",
];

/// Un mensaje para medir, y si con él el usuario empezó o terminó una charla:
/// el saludo se busca donde se empieza y la despedida donde se termina, no en
/// cada mensaje del medio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Medido<'a> {
    pub texto: &'a str,
    pub abre: bool,
    pub cierra: bool,
}

impl<'a> Medido<'a> {
    /// Un mensaje que es toda la charla, como un correo.
    #[must_use]
    pub const fn suelto(texto: &'a str) -> Self {
        Self { texto, abre: true, cierra: true }
    }
}

/// «Tantas veces de tantas».
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Parte {
    pub veces: usize,
    pub de: usize,
}

impl Parte {
    fn sumar(&mut self, si: bool) {
        self.de += 1;
        self.veces += usize::from(si);
    }

    /// El porcentaje, si hay casos suficientes para decir algo.
    #[must_use]
    pub fn porcentaje(self) -> Option<usize> {
        (self.de >= BASE_MINIMA).then(|| self.veces * 100 / self.de)
    }
}

/// Lo que se mide en los mensajes de un lugar y un idioma.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Numeros {
    pub mensajes: usize,
    /// Palabras por mensaje: lo normal y lo largo (nueve de cada diez tienen menos).
    pub palabras_mediana: usize,
    pub palabras_p90: usize,
    pub un_renglon: Parte,
    pub empieza_con_mayuscula: Parte,
    pub todo_en_minusculas: Parte,
    pub termina_con_punto: Parte,
    pub abre_preguntas: Parte,
    pub abre_exclamaciones: Parte,
    pub puntos_suspensivos: Parte,
    pub signos_repetidos: Parte,
    pub emojis: Parte,
    pub emojis_frecuentes: Vec<(String, usize)>,
    pub espacio_doble: Parte,
    pub espacio_antes_de_signo: Parte,
    pub coma_sin_espacio: Parte,
    /// De las palabras que llevan tilde, cuántas la tienen.
    pub tildes: Parte,
    pub sin_tilde: Vec<(String, usize)>,
    pub voseo: usize,
    pub tuteo: usize,
    pub contracciones: Parte,
    pub yo_en_minuscula: Parte,
    pub arranques: Vec<(String, usize)>,
    pub muletillas: Vec<(String, usize)>,
    pub prestadas: Vec<(String, usize)>,
    /// De las veces que empezó una charla, en cuántas saludó.
    pub saludos: Parte,
    /// De las veces que la terminó, en cuántas agradeció o se despidió.
    pub despedidas: Parte,
}

/// Las más repetidas de una cuenta, de mayor a menor (y por orden alfabético
/// si empatan, para que dé siempre lo mismo).
fn mas_repetidas(cuenta: HashMap<String, usize>, minimo: usize) -> Vec<(String, usize)> {
    let mut lista: Vec<(String, usize)> = cuenta.into_iter().filter(|(_, n)| *n >= minimo).collect();
    lista.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    lista.truncate(NOMBRADAS);
    lista
}

const fn es_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x1F1E6..=0x1F1FF | 0x2B50 | 0x2B55)
}

/// Si `frase` (una o varias palabras) está en `palabras` a partir de `desde`.
fn sigue(palabras: &[String], desde: usize, frase: &str) -> bool {
    let partes: Vec<&str> = frase.split(' ').collect();
    palabras.len() >= desde + partes.len() && partes.iter().zip(&palabras[desde..]).all(|(a, b)| a == b)
}

fn contiene(palabras: &[String], frase: &str) -> bool {
    (0..palabras.len()).any(|i| sigue(palabras, i, frase))
}

/// Cuántas veces termina una pregunta (`cierre`) y cuántas de ellas se abrieron (`apertura`).
fn aperturas(texto: &str, apertura: char, cierre: char) -> (usize, usize) {
    let mut cierres = 0;
    let mut anterior = ' ';
    for c in texto.chars() {
        if c == cierre && anterior != cierre {
            cierres += 1;
        }
        anterior = c;
    }
    (texto.chars().filter(|c| *c == apertura).count().min(cierres), cierres)
}

/// Una palabra como la escribió el usuario, en minúsculas y sin los signos de alrededor.
fn palabra_tal_cual(palabra: &str) -> String {
    palabra.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

#[derive(Default)]
struct Cuentas {
    emojis: HashMap<String, usize>,
    sin_tilde: HashMap<String, usize>,
    arranques: HashMap<String, usize>,
    muletillas: HashMap<String, usize>,
    prestadas: HashMap<String, usize>,
}

/// Mayúsculas, renglones y cómo termina.
fn medir_forma(n: &mut Numeros, texto: &str) {
    n.un_renglon.sumar(!texto.contains('\n'));
    if let Some(primera) = texto.chars().find(|c| c.is_alphanumeric())
        && primera.is_alphabetic()
    {
        n.empieza_con_mayuscula.sumar(primera.is_uppercase());
    }
    if texto.chars().filter(|c| c.is_alphabetic()).count() >= 4 {
        n.todo_en_minusculas.sumar(!texto.chars().any(char::is_uppercase));
    }
    n.termina_con_punto.sumar(texto.ends_with('.') && !texto.ends_with(".."));
    n.puntos_suspensivos.sumar(texto.contains("...") || texto.contains('…'));
    n.signos_repetidos.sumar(["??", "!!", "?!", "!?"].iter().any(|s| texto.contains(s)));
}

/// Espacios de más o de menos alrededor de los signos.
fn medir_espacios(n: &mut Numeros, texto: &str) {
    n.espacio_doble.sumar(texto.lines().any(|renglon| renglon.trim().contains("  ")));
    let letras: Vec<char> = texto.chars().collect();
    let mut hay_signo = false;
    let mut con_espacio_antes = false;
    for (i, c) in letras.iter().enumerate() {
        if !matches!(c, ',' | '?' | '!' | ';') {
            continue;
        }
        hay_signo = true;
        if i >= 2 && letras[i - 1] == ' ' && letras[i - 2].is_alphanumeric() {
            con_espacio_antes = true;
        }
        if *c == ','
            && let Some(siguiente) = letras.get(i + 1)
            && !siguiente.is_ascii_digit()
        {
            n.coma_sin_espacio.sumar(siguiente.is_alphabetic());
        }
    }
    if hay_signo {
        n.espacio_antes_de_signo.sumar(con_espacio_antes);
    }
}

fn medir_emojis(n: &mut Numeros, cuentas: &mut Cuentas, texto: &str) {
    let mut hay = false;
    for c in texto.chars().filter(|c| es_emoji(*c)) {
        hay = true;
        *cuentas.emojis.entry(c.to_string()).or_default() += 1;
    }
    n.emojis.sumar(hay);
}

/// Lo que solo tiene sentido en español: signos de apertura, tildes y trato.
fn medir_espanol(n: &mut Numeros, cuentas: &mut Cuentas, texto: &str) {
    let (abiertas, preguntas) = aperturas(texto, '¿', '?');
    n.abre_preguntas.veces += abiertas;
    n.abre_preguntas.de += preguntas;
    let (abiertas, exclamaciones) = aperturas(texto, '¡', '!');
    n.abre_exclamaciones.veces += abiertas;
    n.abre_exclamaciones.de += exclamaciones;

    for palabra in texto.split_whitespace().map(palabra_tal_cual).filter(|p| !p.is_empty()) {
        let largo = palabra.chars().count();
        let con = TILDES.iter().any(|(_, con)| *con == palabra)
            || (largo >= LARGO_DE_TERMINACION && TERMINACIONES_CON_TILDE.iter().any(|(_, con)| palabra.ends_with(con)));
        let sin = TILDES.iter().any(|(sin, _)| *sin == palabra)
            || (largo >= LARGO_DE_TERMINACION && TERMINACIONES_CON_TILDE.iter().any(|(sin, _)| palabra.ends_with(sin)));
        if con || sin {
            n.tildes.sumar(con);
        }
        if sin {
            *cuentas.sin_tilde.entry(palabra.clone()).or_default() += 1;
        }
        n.voseo += usize::from(VOSEO.contains(&palabra.as_str()));
        n.tuteo += usize::from(TUTEO.contains(&palabra.as_str()));
    }
}

/// Lo que solo tiene sentido en inglés: contracciones y la «i» suelta en minúscula.
fn medir_ingles(n: &mut Numeros, texto: &str, palabras: &[String]) {
    for cruda in texto.split_whitespace() {
        let cruda = cruda.trim_matches(|c: char| !c.is_alphanumeric());
        if cruda == "i" || cruda == "I" {
            n.yo_en_minuscula.sumar(cruda == "i");
        }
        let con_apostrofo = cruda.contains(['\'', '’']);
        let pegada: String = cruda.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let es_contraccion = CONTRACCIONES.iter().any(|(corta, _)| *corta == pegada)
            && (con_apostrofo || SIN_APOSTROFO.contains(&pegada.as_str()));
        if es_contraccion {
            n.contracciones.sumar(true);
        }
    }
    for i in 0..palabras.len() {
        if CONTRACCIONES.iter().any(|(_, larga)| sigue(palabras, i, larga)) {
            n.contracciones.sumar(false);
        }
    }
}

/// Con qué arranca, qué expresiones repite y cómo saluda y se despide.
fn medir_expresiones(n: &mut Numeros, cuentas: &mut Cuentas, palabras: &[String], idioma: &str, m: &Medido<'_>) {
    if palabras.len() >= 2
        && let Some(primera) = palabras.first()
        && primera.chars().all(char::is_alphabetic)
        && !ARRANQUES_SIN_INTERES.contains(&primera.as_str())
    {
        *cuentas.arranques.entry(primera.clone()).or_default() += 1;
    }
    for frase in lexico::de_idioma(MULETILLAS, idioma) {
        if contiene(palabras, frase) {
            *cuentas.muletillas.entry((*frase).to_owned()).or_default() += 1;
        }
    }
    if idioma != "en" {
        for palabra in palabras.iter().filter(|p| PRESTADAS.contains(&p.as_str())) {
            *cuentas.prestadas.entry(palabra.clone()).or_default() += 1;
        }
    }
    if m.abre {
        n.saludos.sumar(lexico::de_idioma(SALUDOS, idioma).iter().any(|s| sigue(palabras, 0, s)));
    }
    if m.cierra {
        let cola = &palabras[palabras.len().saturating_sub(COLA)..];
        n.despedidas.sumar(lexico::de_idioma(DESPEDIDAS, idioma).iter().any(|d| contiene(cola, d)));
    }
}

/// Mide los mensajes de un idioma («es», «en», «pt»; con otro, solo lo que
/// vale para cualquiera).
#[must_use]
pub fn medir(mensajes: &[Medido<'_>], idioma: &str) -> Numeros {
    let mut n = Numeros { mensajes: mensajes.len(), ..Numeros::default() };
    let mut cuentas = Cuentas::default();
    let mut largos: Vec<usize> = Vec::with_capacity(mensajes.len());
    for m in mensajes {
        let texto = m.texto.trim();
        if texto.is_empty() {
            continue;
        }
        let palabras = idioma::palabras(texto);
        largos.push(texto.split_whitespace().count());
        medir_forma(&mut n, texto);
        medir_espacios(&mut n, texto);
        medir_emojis(&mut n, &mut cuentas, texto);
        match idioma {
            "es" => medir_espanol(&mut n, &mut cuentas, texto),
            "en" => medir_ingles(&mut n, texto, &palabras),
            _ => {}
        }
        medir_expresiones(&mut n, &mut cuentas, &palabras, idioma, m);
    }
    largos.sort_unstable();
    if let Some(ultimo) = largos.len().checked_sub(1) {
        n.palabras_mediana = largos[largos.len() / 2];
        n.palabras_p90 = largos[(largos.len() * 9 / 10).min(ultimo)];
    }
    n.emojis_frecuentes = mas_repetidas(cuentas.emojis, REPETIDA);
    n.sin_tilde = mas_repetidas(cuentas.sin_tilde, 1);
    n.arranques = mas_repetidas(cuentas.arranques, REPETIDA.max(mensajes.len() / 33));
    n.muletillas = mas_repetidas(cuentas.muletillas, REPETIDA);
    n.prestadas = mas_repetidas(cuentas.prestadas, REPETIDA);
    n
}

/// «casi nunca», «a veces»…, con la cuenta al lado para que se vea de dónde sale.
fn frecuencia(p: Parte) -> Option<String> {
    let palabra = match p.porcentaje()? {
        _ if p.veces == 0 => "nunca",
        _ if p.veces == p.de => "siempre",
        0..=9 => "casi nunca",
        10..=34 => "pocas veces",
        35..=64 => "a veces sí y a veces no",
        _ => "casi siempre",
    };
    Some(format!("{palabra} ({} de {})", p.veces, p.de))
}

/// Una lista como «dale» (8), «che» (3).
fn nombradas(lista: &[(String, usize)]) -> String {
    lista.iter().map(|(palabra, veces)| format!("«{palabra}» ({veces})")).collect::<Vec<_>>().join(", ")
}

/// Solo si pasa más que `desde` por ciento de las veces: lo raro no hace al estilo.
fn si_es_habitual(p: Parte, desde: usize) -> Option<String> {
    p.porcentaje().filter(|x| *x >= desde).and_then(|_| frecuencia(p))
}

fn describir_espanol(n: &Numeros, lineas: &mut Vec<String>) {
    if let Some(f) = frecuencia(n.abre_preguntas) {
        lineas.push(format!("Abre las preguntas con «¿»: {f}."));
    }
    if let Some(f) = frecuencia(n.abre_exclamaciones) {
        lineas.push(format!("Abre las exclamaciones con «¡»: {f}."));
    }
    if let Some(f) = frecuencia(n.tildes) {
        let ejemplos = if n.sin_tilde.is_empty() {
            String::new()
        } else {
            format!(" Escribe sin tilde: {}.", nombradas(&n.sin_tilde))
        };
        lineas.push(format!("Tildes: las pone {f}, contando las palabras que siempre la llevan.{ejemplos}"));
    }
    let (voseo, tuteo) = (n.voseo, n.tuteo);
    if voseo >= 3 && voseo >= tuteo * 3 {
        lineas.push(format!("Trato: de vos («vos», «tenés», «decime»): {voseo} veces contra {tuteo} de tú."));
    } else if tuteo >= 3 && tuteo >= voseo * 3 {
        lineas.push(format!("Trato: de tú («tú», «tienes», «dime»): {tuteo} veces contra {voseo} de vos."));
    }
}

fn describir_ingles(n: &Numeros, lineas: &mut Vec<String>) {
    if let Some(f) = frecuencia(n.contracciones) {
        lineas.push(format!("Contracciones («don't» en vez de «do not»): {f}."));
    }
    if let Some(f) = frecuencia(n.yo_en_minuscula) {
        lineas.push(format!("Escribe «i» en minúscula en vez de «I»: {f}."));
    }
}

/// Lo medido, dicho en renglones para Claude.
#[must_use]
pub fn describir(n: &Numeros, idioma: &str) -> Vec<String> {
    let mut lineas = Vec::new();
    if n.mensajes == 0 {
        return lineas;
    }
    let mut largo = format!(
        "Largo: lo normal son {} palabras por mensaje; uno de cada diez pasa de {}.",
        n.palabras_mediana, n.palabras_p90
    );
    if let Some(f) = frecuencia(n.un_renglon) {
        let _ = write!(largo, " En un solo renglón: {f}.");
    }
    lineas.push(largo);
    if let Some(f) = frecuencia(n.empieza_con_mayuscula) {
        lineas.push(format!("Empieza con mayúscula: {f}."));
    }
    if let Some(f) = si_es_habitual(n.todo_en_minusculas, 35) {
        lineas.push(format!("Mensajes enteros en minúsculas: {f}."));
    }
    if let Some(f) = frecuencia(n.termina_con_punto) {
        lineas.push(format!("Punto al final del mensaje: {f}."));
    }
    match idioma {
        "es" => describir_espanol(n, &mut lineas),
        "en" => describir_ingles(n, &mut lineas),
        _ => {}
    }
    if let Some(f) = si_es_habitual(n.puntos_suspensivos, 10) {
        lineas.push(format!("Puntos suspensivos: {f}."));
    }
    if let Some(f) = si_es_habitual(n.signos_repetidos, 5) {
        lineas.push(format!("Signos repetidos («??», «!!»): {f}."));
    }
    if let Some(f) = si_es_habitual(n.espacio_doble, 10) {
        lineas.push(format!("Deja dos espacios seguidos: {f}."));
    }
    if let Some(f) = si_es_habitual(n.espacio_antes_de_signo, 10) {
        lineas.push(format!("Pone un espacio antes del signo («hola ?»): {f}."));
    }
    if let Some(f) = si_es_habitual(n.coma_sin_espacio, 10) {
        lineas.push(format!("Coma sin espacio después: {f}."));
    }
    if let Some(f) = frecuencia(n.emojis) {
        let habituales = n.emojis.porcentaje().is_some_and(|parte| parte >= EMOJIS_HABITUALES);
        let cuales = if habituales && !n.emojis_frecuentes.is_empty() {
            format!(" Los que más usa: {}.", nombradas(&n.emojis_frecuentes))
        } else {
            String::new()
        };
        lineas.push(format!("Emojis: {f}.{cuales}"));
    }
    if !n.arranques.is_empty() {
        lineas.push(format!("Suele arrancar con: {}.", nombradas(&n.arranques)));
    }
    if !n.muletillas.is_empty() {
        lineas.push(format!("Expresiones que repite: {}.", nombradas(&n.muletillas)));
    }
    if !n.prestadas.is_empty() {
        lineas.push(format!("Palabras en inglés que mete: {}.", nombradas(&n.prestadas)));
    }
    if let Some(f) = frecuencia(n.saludos) {
        lineas.push(format!("Saluda al empezar una charla: {f}."));
    }
    if let Some(f) = frecuencia(n.despedidas) {
        lineas.push(format!("Agradece o se despide al terminarla: {f}."));
    }
    lineas
}

#[cfg(test)]
mod pruebas {
    use super::*;

    /// Cada mensaje como una charla entera.
    fn sueltos<'a>(textos: &[&'a str]) -> Vec<Medido<'a>> {
        textos.iter().map(|texto| Medido::suelto(texto)).collect()
    }

    const RAPIDOS: &[&str] = &[
        "Hace falta todo esto? no alcanza con lo que ya hay?",
        "no me cierra, quiero algo simple y que se entienda solo",
        "deberias sumarlo tambien en la version nueva",
        "dale, hacelo asi y despues me avisas",
        "ok pero si uso otra base que pasa cuando quiera publicarlo?",
        "che vos podes revisar el deploy? tenes el link?",
        "dale, mandame el fix",
    ];

    #[test]
    fn mide_a_quien_escribe_rapido_y_sin_tildes() {
        let n = medir(&sueltos(RAPIDOS), "es");
        assert_eq!(n.mensajes, 7);
        assert_eq!(n.un_renglon, Parte { veces: 7, de: 7 });
        // Solo el primero empieza con mayúscula.
        assert_eq!(n.empieza_con_mayuscula, Parte { veces: 1, de: 7 });
        // Cinco preguntas y ninguna abierta con «¿».
        assert_eq!(n.abre_preguntas, Parte { veces: 0, de: 5 });
        // deberias, tambien, version, asi, despues, podes, tenes: ninguna con tilde.
        assert_eq!(n.tildes, Parte { veces: 0, de: 7 });
        assert!(n.sin_tilde.iter().any(|(p, _)| p == "tambien"), "{:?}", n.sin_tilde);
        assert!(n.voseo >= 4, "vos, podes, tenes, hacelo: {}", n.voseo);
        assert_eq!(n.tuteo, 0);
        assert_eq!(n.termina_con_punto.veces, 0);
        assert_eq!(n.emojis.veces, 0);
        assert_eq!(n.arranques, vec![("dale".to_owned(), 2)]);
        assert_eq!(n.muletillas.first(), Some(&("dale".to_owned(), 2)));
        assert_eq!(n.prestadas, Vec::new(), "cada una aparece una sola vez");

        let dicho = describir(&n, "es").join("\n");
        assert!(dicho.contains("Abre las preguntas con «¿»: nunca (0 de 5)."), "{dicho}");
        assert!(dicho.contains("Tildes: las pone nunca (0 de 7)"), "{dicho}");
        assert!(dicho.contains("Trato: de vos"), "{dicho}");
        assert!(dicho.contains("Suele arrancar con: «dale» (2)."), "{dicho}");
    }

    #[test]
    fn mide_a_quien_escribe_con_cuidado() {
        let cuidados = [
            "Hola, ¿cómo estás? Te escribo por la versión nueva.",
            "Buenas tardes. ¿Podrías revisar el código cuando puedas? Muchas gracias.",
            "Hola: también habría que mirar la página de inicio. ¡Gracias!",
            "Estimado: después de la reunión te mando el número. Saludos.",
            "Hola, ¿tienes un minuto? Dime cuándo puedes y hazlo con calma. ¿Quieres que te llame?",
            "¿Tú puedes mañana? Eres el único que lo sabe.",
        ];
        let n = medir(&sueltos(&cuidados), "es");
        // El último empieza con «¿»: cuenta la primera letra.
        assert_eq!(n.empieza_con_mayuscula, Parte { veces: 6, de: 6 });
        assert_eq!(n.abre_preguntas.veces, n.abre_preguntas.de);
        assert_eq!(n.tildes.veces, n.tildes.de, "{:?}", n.sin_tilde);
        assert!(n.tildes.de >= 6);
        assert_eq!(n.saludos.veces, 5);
        assert_eq!(n.despedidas.veces, 3);
        assert!(n.tuteo >= 5 && n.voseo == 0, "{} / {}", n.tuteo, n.voseo);
        let dicho = describir(&n, "es").join("\n");
        assert!(dicho.contains("Trato: de tú"), "{dicho}");
        assert!(dicho.contains("Saluda al empezar una charla: casi siempre (5 de 6)."), "{dicho}");
    }

    #[test]
    fn el_saludo_se_mide_donde_empieza_la_charla() {
        // Una charla de seis mensajes: saluda en el primero y agradece en el último.
        let charla =
            ["hola, como va?", "te queria preguntar algo", "tenes el link?", "ah ya lo vi", "joya", "gracias!"];
        let mut medidos = sueltos(&charla);
        for (i, m) in medidos.iter_mut().enumerate() {
            m.abre = i == 0;
            m.cierra = i == charla.len() - 1;
        }
        // Cinco charlas iguales: saludó las cinco veces que empezó, no «una de cada seis».
        let cinco: Vec<Medido<'_>> = (0..5).flat_map(|_| medidos.clone()).collect();
        let n = medir(&cinco, "es");
        assert_eq!(n.mensajes, 30);
        assert_eq!(n.saludos, Parte { veces: 5, de: 5 });
        assert_eq!(n.despedidas, Parte { veces: 5, de: 5 });
        let dicho = describir(&n, "es").join("\n");
        assert!(dicho.contains("Saluda al empezar una charla: siempre (5 de 5)."), "{dicho}");
        assert!(dicho.contains("Agradece o se despide al terminarla: siempre (5 de 5)."), "{dicho}");
    }

    #[test]
    fn mide_espacios_signos_y_emojis() {
        let mensajes = [
            "hola ,como va ?",
            "todo bien!!  dale 🙂",
            "mmm... no se,despues vemos 🙂",
            "ok 🙂 🚀",
            "1,5 no cuenta como coma sin espacio",
        ];
        let n = medir(&sueltos(&mensajes), "es");
        assert_eq!(n.espacio_doble.veces, 1);
        // De los cuatro mensajes que tienen algún signo.
        assert_eq!(n.espacio_antes_de_signo, Parte { veces: 1, de: 4 });
        assert_eq!(n.coma_sin_espacio, Parte { veces: 2, de: 2 });
        assert_eq!(n.signos_repetidos.veces, 1);
        assert_eq!(n.puntos_suspensivos.veces, 1);
        assert_eq!(n.emojis, Parte { veces: 3, de: 5 });
        assert_eq!(n.emojis_frecuentes, vec![("🙂".to_owned(), 3)]);
        assert!(describir(&n, "es").join("\n").contains("Los que más usa: «🙂» (3)."));
        // De alguien que casi no usa emojis no se nombra ninguno.
        let mut casi_sin: Vec<&str> = vec!["dale, despues lo vemos"; 40];
        casi_sin.extend(["joya 🙂", "listo 🙂"]);
        let pocos = describir(&medir(&sueltos(&casi_sin), "es"), "es").join("\n");
        assert!(pocos.contains("Emojis: casi nunca (2 de 42).") && !pocos.contains("Los que más usa"), "{pocos}");
    }

    #[test]
    fn mide_lo_propio_del_ingles() {
        let mensajes = [
            "i don't think it's ready, can you check?",
            "I do not want to push this yet",
            "thats fine, i will fix it and let you know. thanks",
            "hey, im not sure about its name",
            "hi, can you review it? cheers",
        ];
        let n = medir(&sueltos(&mensajes), "en");
        // don't, it's, thats, im frente a «do not»; el «its» sin apóstrofo no cuenta.
        assert_eq!(n.contracciones, Parte { veces: 4, de: 5 });
        assert_eq!(n.yo_en_minuscula, Parte { veces: 2, de: 3 });
        assert_eq!(n.saludos.veces, 2);
        assert_eq!(n.despedidas.veces, 2);
        assert_eq!(n.tildes.de, 0, "las tildes son del español");
        assert_eq!(n.prestadas, Vec::new());
    }

    #[test]
    fn sin_mensajes_no_dice_nada() {
        let n = medir(&[], "es");
        assert_eq!(n, Numeros::default());
        assert_eq!(describir(&n, "es"), Vec::<String>::new());
        // «Nunca» y «siempre» son exactos: con una sola excepción ya es «casi».
        assert_eq!(frecuencia(Parte { veces: 6, de: 700 }).unwrap(), "casi nunca (6 de 700)");
        assert_eq!(frecuencia(Parte { veces: 699, de: 700 }).unwrap(), "casi siempre (699 de 700)");
        assert_eq!(frecuencia(Parte { veces: 700, de: 700 }).unwrap(), "siempre (700 de 700)");
        assert_eq!(frecuencia(Parte { veces: 0, de: 700 }).unwrap(), "nunca (0 de 700)");
        assert_eq!(frecuencia(Parte { veces: 3, de: 4 }), None);
        // Con pocos casos no saca porcentajes.
        let pocos = medir(&sueltos(&["hola", "ok"]), "es");
        assert_eq!(pocos.un_renglon.porcentaje(), None);
        assert_eq!(describir(&pocos, "es").len(), 1, "solo el largo");
    }
}
