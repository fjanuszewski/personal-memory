//! En qué idioma está un mensaje. Sin modelo: cuenta las palabras más comunes
//! de cada idioma. Alcanza para mensajes de chat; si el mensaje es muy corto
//! para saberlo («ok», «dale»), queda sin idioma.

use crate::texto;

/// Mensaje demasiado corto, o parejo entre dos idiomas.
pub const SIN_IDIOMA: &str = "";

/// Las palabras más comunes de cada idioma, en minúsculas y sin acentos. Las
/// que comparten dos idiomas («que», «de», «para») están en los dos: así no
/// inclinan la cuenta.
const COMUNES: &[(&str, &[&str])] = &[
    (
        "es",
        &[
            "a", "aca", "ahora", "al", "algo", "asi", "bien", "cada", "como", "con", "cual", "cuando", "dale", "de",
            "deberia", "del", "desde", "despues", "donde", "el", "en", "entre", "es", "esa", "ese", "eso", "esta",
            "estan", "este", "esto", "estoy", "fue", "gracias", "hacer", "hasta", "hay", "hola", "la", "las", "le",
            "lo", "los", "mas", "me", "mi", "muy", "nada", "necesito", "no", "o", "otra", "otro", "para", "pero",
            "podes", "por", "porque", "puede", "que", "quien", "quiero", "se", "ser", "si", "sin", "sobre", "son",
            "sos", "su", "tambien", "te", "tenes", "tengo", "todo", "un", "una", "vos", "y", "ya",
        ],
    ),
    (
        "en",
        &[
            "a", "about", "all", "also", "an", "and", "any", "are", "as", "at", "be", "been", "but", "by", "can",
            "could", "did", "do", "does", "for", "from", "had", "has", "have", "here", "how", "i", "if", "in", "into",
            "is", "it", "its", "just", "let", "my", "need", "not", "now", "of", "on", "or", "please", "should", "so",
            "than", "thanks", "that", "the", "them", "then", "there", "these", "they", "this", "those", "to", "want",
            "was", "we", "what", "when", "where", "which", "why", "will", "with", "would", "yes", "you", "your",
        ],
    ),
    (
        "pt",
        &[
            "a", "agora", "ao", "aqui", "as", "ate", "bem", "bom", "com", "da", "das", "de", "do", "dos", "e", "ela",
            "ele", "em", "entao", "estou", "eu", "fazer", "foi", "isso", "ja", "mais", "mas", "meu", "minha", "muito",
            "na", "nada", "nao", "no", "nos", "o", "obrigada", "obrigado", "ola", "os", "ou", "para", "pela", "pelo",
            "pode", "por", "quando", "que", "sao", "se", "ser", "seu", "sim", "sua", "tambem", "tem", "tudo", "um",
            "uma", "voce", "vou",
        ],
    ),
];

/// Con menos palabras comunes que estas no se arriesga un idioma.
const MINIMAS: usize = 2;

/// El texto en minúsculas y sin acentos ni tildes de otros idiomas («não»
/// queda «nao»), para comparar con las listas de acá.
#[must_use]
pub fn plano(texto: &str) -> String {
    texto::llano(texto)
        .chars()
        .map(|c| match c {
            'ã' => 'a',
            'õ' => 'o',
            'ç' => 'c',
            otro => otro,
        })
        .collect()
}

/// Las palabras de un texto, en minúsculas y sin acentos.
#[must_use]
pub fn palabras(texto: &str) -> Vec<String> {
    plano(texto).split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(str::to_owned).collect()
}

/// El idioma de un texto («es», «en», «pt») o [`SIN_IDIOMA`] si no se puede saber.
#[must_use]
pub fn de(texto: &str) -> &'static str {
    let palabras = palabras(texto);
    let mut cuentas: Vec<(usize, &'static str)> = COMUNES
        .iter()
        .map(|(idioma, comunes)| (palabras.iter().filter(|p| comunes.contains(&p.as_str())).count(), *idioma))
        .collect();
    cuentas.sort_by_key(|cuenta| std::cmp::Reverse(cuenta.0));
    match cuentas[..] {
        [(primero, idioma), (segundo, _), ..] if primero >= MINIMAS && primero > segundo => idioma,
        _ => SIN_IDIOMA,
    }
}

/// El código de un idioma dicho de cualquier forma: «español», «Spanish»,
/// «es», «en-US». `None` si no se reconoce.
#[must_use]
pub fn leer(texto: &str) -> Option<&'static str> {
    let llano = plano(texto.trim());
    let base = llano.split(['-', '_']).next().unwrap_or_default();
    match base {
        "es" | "español" | "espanol" | "castellano" | "spanish" => Some("es"),
        "en" | "ingles" | "english" => Some("en"),
        "pt" | "portugues" | "portuguese" => Some("pt"),
        _ => None,
    }
}

/// Cómo se nombra un idioma en lo que se le muestra a Claude.
#[must_use]
pub fn nombre(codigo: &str) -> &str {
    match codigo {
        "es" => "español",
        "en" => "inglés",
        "pt" => "portugués",
        otro => otro,
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn reconoce_el_idioma_de_un_mensaje() {
        assert_eq!(de("no me convence del todo, quiero algo que se entienda sin explicarlo"), "es");
        assert_eq!(de("Hace falta todo esto? no alcanza con lo que ya hay?"), "es");
        assert_eq!(de("can you check if the deploy is done and let me know"), "en");
        assert_eq!(de("você pode me dizer quando o deploy vai ficar pronto? obrigado"), "pt");
    }

    #[test]
    fn lo_muy_corto_o_parejo_queda_sin_idioma() {
        assert_eq!(de("ok dale"), SIN_IDIOMA);
        assert_eq!(de("Try again"), SIN_IDIOMA);
        assert_eq!(de(""), SIN_IDIOMA);
        assert_eq!(de("deploy fix 0.6.5"), SIN_IDIOMA);
    }

    #[test]
    fn lee_el_idioma_dicho_de_varias_formas() {
        assert_eq!(leer("Español"), Some("es"));
        assert_eq!(leer(" en-US "), Some("en"));
        assert_eq!(leer("portugués"), Some("pt"));
        assert_eq!(leer("klingon"), None);
        assert_eq!(nombre("es"), "español");
        assert_eq!(nombre("xx"), "xx");
    }
}
