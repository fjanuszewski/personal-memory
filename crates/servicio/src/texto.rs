//! Palabras: cómo se reduce un texto a las raíces que se guardan y se buscan.
//!
//! Cada palabra se pasa a minúsculas, se le sacan los acentos y se la recorta
//! a su raíz («entregas» y «entregamos» dan «entreg»), así una búsqueda
//! encuentra lo mismo escrito con otra terminación o sin acentos. Como
//! recortar con y sin acento da raíces distintas («facturación» da «factur»
//! y «facturacion» queda igual), se guardan las dos: quien escribe sin
//! acentos encuentra igual lo que se guardó con ellos.

use std::collections::HashSet;
use std::sync::LazyLock;

use rust_stemmers::{Algorithm, Stemmer};

static RAIZ: LazyLock<Stemmer> = LazyLock::new(|| Stemmer::create(Algorithm::Spanish));

/// Palabras que no dicen de qué trata un texto (sin acentos).
const VACIAS: &[&str] = &[
    "a", "al", "algo", "algun", "alguna", "algunas", "alguno", "algunos", "ante", "antes", "aquel", "aquella", "aqui",
    "como", "con", "contra", "cual", "cuales", "cuando", "cuanta", "cuantas", "cuanto", "cuantos", "de", "del",
    "desde", "donde", "durante", "e", "el", "ella", "ellas", "ellos", "en", "entre", "era", "eran", "eres", "es",
    "esa", "esas", "ese", "eso", "esos", "esta", "estaba", "estaban", "estado", "estamos", "estan", "estar", "estas",
    "este", "esto", "estos", "estoy", "fue", "fueron", "fui", "ha", "habia", "habian", "han", "has", "hasta", "hay",
    "he", "hemos", "la", "las", "le", "les", "lo", "los", "mas", "me", "mi", "mis", "mucho", "muchos", "muy", "nada",
    "ni", "no", "nos", "nosotros", "o", "os", "otra", "otras", "otro", "otros", "para", "pero", "por", "porque", "que",
    "quien", "quienes", "se", "sea", "ser", "si", "sido", "sin", "sobre", "sois", "somos", "son", "soy", "su", "sus",
    "tambien", "tanto", "te", "tiene", "tienen", "todo", "todos", "tu", "tus", "u", "un", "una", "uno", "unos", "vos",
    "y", "ya", "yo",
];

/// Palabras que acá se usan para lo mismo aunque no se parezcan: quien
/// pregunta por «los correos» quiere también lo que se guardó como «mail» o
/// «emails». Cada renglón trae las formas en singular y en plural (sin
/// acentos), en el mismo orden. Vale solo para lo que se busca: lo guardado
/// no cambia, así que no hay que armar de nuevo el índice de palabras.
const IGUALES: &[(&[&str], &[&str])] = &[(&["correo", "mail", "email"], &["correos", "mails", "emails"])];

/// El renglón de [`IGUALES`] donde está una palabra (en minúsculas y sin
/// acentos), y si está en plural.
fn iguales(plana: &str) -> Option<(usize, bool)> {
    IGUALES.iter().enumerate().find_map(|(renglon, (una, varias))| {
        if una.contains(&plana) {
            Some((renglon, false))
        } else if varias.contains(&plana) {
            Some((renglon, true))
        } else {
            None
        }
    })
}

fn sin_acento(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        otro => otro,
    }
}

/// Un texto de muestra con palabras de todo tipo: junto con la lista de
/// palabras vacías, deja ver si cambió la forma de sacar las raíces.
const MUESTRA: &str = "Las entregas son los viernes; la facturación de octubre se envió al cliente y quedaron \
    pendientes dos reuniones. Deploy en us-east-2 con la versión 3.5 del pipeline: corrigiendo errores de sesión, \
    contraseñas vencidas rápidamente y páginas que cargaban muy lentas. ¿Cuándo migramos la base? Decidimos que el \
    próximo jueves, después de probarlo todo en el ambiente de pruebas.";

/// Una marca corta de un texto: dos textos iguales dan siempre la misma, y
/// dos distintos, en la práctica, nunca. Sirve para no guardar dos veces el
/// mismo pedazo de conversación.
#[must_use]
pub fn marca(texto: &str) -> String {
    // Dos pasadas de FNV-1a de 64 bits, cada una con su punto de partida.
    let pasada =
        |inicio: u64| texto.bytes().fold(inicio, |h, byte| (h ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{:016x}{:016x}", pasada(0xcbf2_9ce4_8422_2325), pasada(0x6c62_272e_07bb_0142))
}

/// Identifica la forma de sacar las raíces. Si cambia (otra lista de
/// palabras vacías, otro recorte), lo guardado en el índice de palabras ya
/// no sirve y hay que armarlo de nuevo.
#[must_use]
pub fn version() -> String {
    marca(&para_indexar(&format!("{MUESTRA} {}", VACIAS.join(" "))))
}

/// El texto en minúsculas y sin acentos.
#[must_use]
pub fn llano(texto: &str) -> String {
    texto.to_lowercase().chars().map(sin_acento).collect()
}

/// Las raíces de una palabra ya en minúsculas: una, o dos si recortarla con
/// y sin acento da distinto. Vacío si la palabra no dice nada.
fn raices_de(palabra: &str) -> Vec<String> {
    let plana: String = palabra.chars().map(sin_acento).collect();
    if VACIAS.contains(&plana.as_str()) {
        return Vec::new();
    }
    // Un número o un identificador («751», «us2») va tal cual.
    if plana.chars().any(|c| c.is_ascii_digit()) {
        return vec![plana];
    }
    if plana.chars().count() < 2 {
        return Vec::new();
    }
    let con_acento: String = RAIZ.stem(palabra).chars().map(sin_acento).collect();
    let sin: String = RAIZ.stem(&plana).into_owned();
    if con_acento == sin { vec![sin] } else { vec![con_acento, sin] }
}

/// Las palabras de un texto, en orden: cada una con el lugar que ocupa en el
/// texto (en bytes) y sus raíces.
fn recorrer(texto: &str) -> Vec<(usize, usize, Vec<String>)> {
    let mut palabras = Vec::new();
    let mut inicio: Option<usize> = None;
    let cerrar = |desde: usize, hasta: usize, palabras: &mut Vec<(usize, usize, Vec<String>)>| {
        palabras.push((desde, hasta, raices_de(&texto[desde..hasta].to_lowercase())));
    };
    for (i, c) in texto.char_indices() {
        match (c.is_alphanumeric(), inicio) {
            (true, None) => inicio = Some(i),
            (false, Some(desde)) => {
                cerrar(desde, i, &mut palabras);
                inicio = None;
            }
            _ => {}
        }
    }
    if let Some(desde) = inicio {
        cerrar(desde, texto.len(), &mut palabras);
    }
    palabras
}

/// Lo que se guarda en el índice de palabras: las raíces del texto, en orden.
#[must_use]
pub fn para_indexar(texto: &str) -> String {
    let raices: Vec<String> = recorrer(texto).into_iter().flat_map(|(_, _, r)| r).collect();
    raices.join(" ")
}

/// Las raíces sueltas de un texto ya indexado, para ver cuáles tiene.
#[must_use]
pub fn conjunto(indexado: &str) -> HashSet<&str> {
    indexado.split_whitespace().collect()
}

/// La consulta con sus palabras equivalentes a la vista, para buscar también
/// por el significado de esa otra forma de decirla: «los correos» pasa a
/// «los correos (mails, emails)». Un modelo de huellas puede leer «correos»
/// como cartas y dejar lejos la nota que habla de mails. `None` si la
/// consulta no tiene ninguna palabra con equivalentes.
#[must_use]
pub fn con_equivalentes(texto: &str) -> Option<String> {
    let mut salida = String::with_capacity(texto.len() + 32);
    let mut copiado = 0;
    // Cada renglón se aclara una sola vez, en la primera palabra que lo nombra.
    let mut aclarados: Vec<usize> = Vec::new();
    for (desde, hasta, _) in recorrer(texto) {
        let plana = llano(&texto[desde..hasta]);
        let Some((renglon, plural)) = iguales(&plana) else { continue };
        if aclarados.contains(&renglon) {
            continue;
        }
        aclarados.push(renglon);
        let (una, varias) = IGUALES[renglon];
        let formas = if plural { varias } else { una };
        let otras: Vec<&str> = formas.iter().copied().filter(|forma| *forma != plana).collect();
        salida.push_str(&texto[copiado..hasta]);
        salida.push_str(" (");
        salida.push_str(&otras.join(", "));
        salida.push(')');
        copiado = hasta;
    }
    if aclarados.is_empty() {
        return None;
    }
    salida.push_str(&texto[copiado..]);
    Some(salida)
}

/// Lo que se busca: las palabras de la consulta que dicen algo, cada una con
/// sus raíces. Una palabra repetida cuenta una vez, y una que tiene
/// equivalentes («correos», «mails») trae también las raíces de ellos.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Consulta {
    palabras: Vec<Vec<String>>,
}

impl Consulta {
    #[must_use]
    pub fn de(texto: &str) -> Self {
        let mut palabras: Vec<Vec<String>> = Vec::new();
        for (desde, hasta, mut raices) in recorrer(texto) {
            if raices.is_empty() {
                continue;
            }
            if let Some((renglon, _)) = iguales(&llano(&texto[desde..hasta])) {
                // Las raíces de todo el renglón, siempre en el mismo orden:
                // así «correos» y «mails» cuentan como la misma palabra.
                let (una, varias) = IGUALES[renglon];
                raices.clear();
                for raiz in una.iter().chain(varias).flat_map(|forma| raices_de(forma)) {
                    if !raices.contains(&raiz) {
                        raices.push(raiz);
                    }
                }
            }
            if !palabras.contains(&raices) {
                palabras.push(raices);
            }
        }
        Self { palabras }
    }

    /// Cuántas palabras tiene la consulta.
    #[must_use]
    pub fn total(&self) -> usize {
        self.palabras.len()
    }

    /// Cuántas de esas palabras tiene que tener un texto para que la
    /// coincidencia sea firme: todas si son hasta tres, y siete de cada diez
    /// si son más (una pregunta larga casi nunca aparece entera).
    #[must_use]
    pub fn necesarias(&self) -> usize {
        let total = self.total();
        if total <= 3 { total.max(1) } else { (total * 7).div_ceil(10) }
    }

    /// Cuántas palabras de la consulta están en un texto, dadas sus raíces.
    #[must_use]
    pub fn coinciden(&self, raices: &HashSet<&str>) -> usize {
        self.palabras.iter().filter(|p| p.iter().any(|r| raices.contains(r.as_str()))).count()
    }

    /// Si un texto (sus raíces) tiene suficientes palabras de la consulta.
    #[must_use]
    pub fn firme(&self, raices: &HashSet<&str>) -> bool {
        self.total() > 0 && self.coinciden(raices) >= self.necesarias()
    }

    /// La consulta para el índice de palabras de SQLite: con `todas`, textos
    /// que tengan todas las palabras; si no, alguna. `None` si no quedó nada
    /// que buscar.
    #[must_use]
    pub fn para_buscar(&self, todas: bool) -> Option<String> {
        if self.palabras.is_empty() {
            return None;
        }
        let grupos: Vec<String> = self
            .palabras
            .iter()
            .map(|p| format!("({})", p.iter().map(|r| format!("\"{r}\"")).collect::<Vec<_>>().join(" OR ")))
            .collect();
        Some(grupos.join(if todas { " AND " } else { " OR " }))
    }

    /// Las palabras de la consulta, cada una con sus raíces.
    #[must_use]
    pub fn palabras(&self) -> &[Vec<String>] {
        &self.palabras
    }

    /// Como [`Self::para_buscar`] sin `todas`, pero pidiendo además que el
    /// texto tenga al menos una de las palabras de `obligatorias` (por su
    /// lugar en la consulta).
    #[must_use]
    pub fn para_buscar_con(&self, obligatorias: &[usize]) -> Option<String> {
        let alguna = self.para_buscar(false)?;
        let de_estas: Vec<String> = obligatorias
            .iter()
            .filter_map(|lugar| self.palabras.get(*lugar))
            .flat_map(|raices| raices.iter().map(|r| format!("\"{r}\"")))
            .collect();
        if de_estas.is_empty() {
            return Some(alguna);
        }
        Some(format!("({}) AND ({alguna})", de_estas.join(" OR ")))
    }

    /// Las palabras, cada una por su primera raíz, ordenadas: reconoce la
    /// misma búsqueda aunque se escriba en otro orden o con otra terminación.
    #[must_use]
    pub fn firma(&self) -> String {
        let mut raices: Vec<&str> = self.palabras.iter().filter_map(|p| p.first()).map(String::as_str).collect();
        raices.sort_unstable();
        raices.dedup();
        raices.join(" ")
    }

    fn toca(&self, raices: &[String]) -> bool {
        self.palabras.iter().any(|p| p.iter().any(|r| raices.contains(r)))
    }
}

/// El pedazo de `texto` donde más aparece lo buscado, de hasta `palabras`
/// palabras. Si no aparece, el comienzo.
#[must_use]
pub fn extracto(texto: &str, consulta: &Consulta, palabras: usize) -> String {
    let todas = recorrer(texto);
    if todas.len() <= palabras {
        return texto.trim().to_owned();
    }
    let aciertos: Vec<usize> = todas.iter().map(|(_, _, r)| usize::from(consulta.toca(r))).collect();
    let mut cuenta: usize = aciertos[..palabras].iter().sum();
    let (mut mejor, mut desde) = (cuenta, 0);
    for i in palabras..todas.len() {
        cuenta = cuenta + aciertos[i] - aciertos[i - palabras];
        if cuenta > mejor {
            mejor = cuenta;
            desde = i + 1 - palabras;
        }
    }
    // La ventana se acomoda para que lo encontrado quede en el medio.
    let hay = |i: &usize| aciertos[*i] == 1;
    let ventana = desde..desde + palabras;
    if let (true, Some(primero), Some(ultimo)) = (mejor > 0, ventana.clone().find(hay), ventana.rev().find(hay)) {
        let sobra = palabras - (ultimo - primero + 1);
        desde = primero.saturating_sub(sobra / 2).min(todas.len() - palabras);
    }
    let hasta = desde + palabras;
    let pedazo = texto[todas[desde].0..todas[hasta - 1].1].trim();
    format!("{}{pedazo}{}", if desde > 0 { "… " } else { "" }, if hasta < todas.len() { " …" } else { "" })
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn reduce_las_palabras_a_sus_raices() {
        assert_eq!(para_indexar("Las entregas son los viernes"), "entreg viern");
        // Con acento y sin acento se guardan las dos raíces: se encuentra igual.
        assert_eq!(para_indexar("Facturación"), "factur facturacion");
        assert_eq!(para_indexar("facturacion"), "facturacion");
        assert_eq!(para_indexar("Se facturó"), "factur");
        // Números e identificadores van tal cual, partidos en sus piezas.
        assert_eq!(para_indexar("PORTAL-751 en us-east-2 (12/11)"), "portal 751 us east 2 12 11");
        assert_eq!(para_indexar("¿Qué es lo que hay?"), "");
        assert_eq!(para_indexar("El ñandú y la sesión de Cognito"), "ñandu sesion cognit");
    }

    #[test]
    fn la_marca_distingue_textos() {
        assert_eq!(marca("hola"), marca("hola"));
        assert_ne!(marca("hola"), marca("hola "));
        assert_ne!(marca("ab"), marca("ba"));
        assert_eq!(marca("").len(), 32);
        // El programa que trae datos de otra base calcula la misma marca por su cuenta.
        assert_eq!(marca("hola"), "4029fbcc7e6d3137a113f42a3276c574");
        assert_eq!(marca("Usuario: ¿Con qué mandamos los mails del Inbox?"), "a0dd755cc0caacc40f944aae07513791");
        assert_eq!(version(), version());
    }

    #[test]
    fn arma_la_consulta_para_el_indice() {
        let c = Consulta::de("¿cuándo se entrega la facturación?");
        assert_eq!(c.total(), 2);
        assert_eq!(c.para_buscar(true).as_deref(), Some("(\"entreg\") AND (\"factur\" OR \"facturacion\")"));
        assert_eq!(c.para_buscar(false).as_deref(), Some("(\"entreg\") OR (\"factur\" OR \"facturacion\")"));
        assert_eq!(Consulta::de("¿y qué hay?").para_buscar(false), None);
        // Una palabra repetida cuenta una vez.
        assert_eq!(Consulta::de("login, login y más login").total(), 1);
        // Con palabras obligatorias: alguna de esas, y cualquiera de todas.
        let larga = Consulta::de("deploy región producción");
        assert_eq!(
            larga.para_buscar_con(&[1]).as_deref(),
            Some("(\"region\") AND ((\"deploy\") OR (\"region\") OR (\"produccion\"))")
        );
        assert_eq!(larga.para_buscar_con(&[]), larga.para_buscar(false));
    }

    #[test]
    fn correos_mails_y_emails_son_la_misma_palabra() {
        // Al guardar, cada una queda con su raíz: no se parecen entre sí.
        assert_eq!(para_indexar("correos mail mails email emails e-mail"), "corre mail mails email emails mail");
        // Al buscar, cualquiera de ellas trae las raíces de todas.
        let c = Consulta::de("con qué servicio mandamos los correos");
        assert_eq!(c.total(), 3);
        assert_eq!(
            c.para_buscar(false).as_deref(),
            Some("(\"servici\") OR (\"mand\") OR (\"corre\" OR \"mail\" OR \"email\" OR \"mails\" OR \"emails\")")
        );
        for nota in ["El Inbox manda mail con SendGrid", "Los mails salen por Mailgun", "Proveedor de email: SES"] {
            let indexada = para_indexar(nota);
            assert!(Consulta::de("correos").firme(&conjunto(&indexada)), "{nota}");
            assert!(Consulta::de("Correo").firme(&conjunto(&indexada)), "{nota}");
        }
        let en_castellano = para_indexar("El correo pasa a SES");
        assert!(Consulta::de("mails").firme(&conjunto(&en_castellano)));
        assert!(Consulta::de("e-mail").firme(&conjunto(&en_castellano)));
        // Dichas de dos formas en la misma consulta cuentan una vez, y la
        // búsqueda se reconoce como la misma.
        assert_eq!(Consulta::de("correos y mails del Inbox").total(), 2);
        assert_eq!(Consulta::de("mails del inbox").firma(), Consulta::de("correos del inbox").firma());
        // Lo que no tiene equivalentes queda como estaba: «corre» no es «correo».
        assert_eq!(Consulta::de("dónde corre").para_buscar(false).as_deref(), Some("(\"corr\")"));
        assert!(!Consulta::de("dónde corre").firme(&conjunto(&en_castellano)));
    }

    #[test]
    fn deja_los_equivalentes_a_la_vista() {
        assert_eq!(
            con_equivalentes("con qué servicio mandamos los correos").as_deref(),
            Some("con qué servicio mandamos los correos (mails, emails)")
        );
        assert_eq!(con_equivalentes("¿Cómo sale el Mail?").as_deref(), Some("¿Cómo sale el Mail (correo, email)?"));
        // Cada grupo de equivalentes se aclara una sola vez.
        assert_eq!(
            con_equivalentes("correos o emails del cliente").as_deref(),
            Some("correos (mails, emails) o emails del cliente")
        );
        assert_eq!(con_equivalentes("el problema del login"), None);
        assert_eq!(con_equivalentes(""), None);
    }

    #[test]
    fn cuenta_cuantas_palabras_coinciden() {
        let c = Consulta::de("facturacion de octubre globex");
        let nota = para_indexar("Se facturó en octubre: 62 horas");
        let raices = conjunto(&nota);
        // «facturacion» sin acento no llega a «factur»: coincide solo «octubre».
        assert_eq!(c.coinciden(&raices), 1);
        let con_acento = Consulta::de("facturación de octubre globex");
        assert_eq!(con_acento.coinciden(&raices), 2);
        assert!(!con_acento.firme(&raices), "son tres palabras: hacen falta las tres");
        assert!(Consulta::de("facturó octubre").firme(&raices));
        assert!(!Consulta::de("").firme(&raices));
    }

    #[test]
    fn cuantas_palabras_hacen_falta() {
        let de = |n: usize| Consulta { palabras: (0..n).map(|i| vec![format!("p{i}")]).collect() }.necesarias();
        let pares = [(0, 1), (1, 1), (2, 2), (3, 3), (4, 3), (5, 4), (6, 5), (8, 6), (10, 7)];
        for (total, esperadas) in pares {
            assert_eq!(de(total), esperadas, "de {total}");
        }
    }

    #[test]
    fn la_firma_no_depende_del_orden_ni_de_la_terminacion() {
        assert_eq!(Consulta::de("cuándo son las entregas del viernes").firma(), "entreg viern");
        assert_eq!(Consulta::de("viernes entrega").firma(), "entreg viern");
        assert_eq!(Consulta::de("¿y?").firma(), "");
    }

    #[test]
    fn muestra_el_pedazo_donde_aparece_lo_buscado() {
        let largo = format!("{} La sesión de Cognito vencía cada hora. {}", "relleno ".repeat(60), "cola ".repeat(60));
        let e = extracto(&largo, &Consulta::de("sesion cognito"), 12);
        assert!(e.starts_with("… ") && e.ends_with(" …"), "{e}");
        assert!(e.contains("La sesión de Cognito vencía"), "{e}");
        // Si no aparece, el comienzo.
        let e = extracto(&largo, &Consulta::de("kubernetes"), 5);
        assert_eq!(e, "relleno relleno relleno relleno relleno …");
        // Un texto corto va entero.
        assert_eq!(extracto("  Corto y entero.  ", &Consulta::de("nada"), 12), "Corto y entero.");
    }
}
