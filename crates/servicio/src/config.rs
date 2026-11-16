//! Configuración del servicio, leída del entorno con valores por defecto
//! pensados para una Mac.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::estado::{NOMBRE_POR_DEFECTO, Umbrales};

/// Quién calcula las huellas de significado.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proveedor {
    /// Un modelo que corre dentro del servicio.
    Interno,
    /// Ollama, en esta misma máquina.
    Ollama,
}

/// WhatsApp como fuente para aprender cómo escribe el usuario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhatsApp {
    /// La base de WhatsApp Desktop (`MEMORIA_WHATSAPP`; por defecto, donde la
    /// deja la aplicación).
    pub base: PathBuf,
    /// Chats que quedan afuera: los que tienen alguno de estos textos en el
    /// nombre (`MEMORIA_WHATSAPP_SIN`, separados por punto y coma).
    pub sin: Vec<String>,
}

/// De dónde más, aparte de las conversaciones con Claude, se aprende cómo
/// escribe el usuario. Cada fuente se enciende aparte; apagada, no se lee.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fuentes {
    pub whatsapp: Option<WhatsApp>,
}

impl Fuentes {
    /// Si hay alguna fuente encendida.
    #[must_use]
    pub const fn alguna(&self) -> bool {
        self.whatsapp.is_some()
    }
}

/// Dónde vive cada cosa.
#[derive(Clone, Debug)]
pub struct Config {
    /// El archivo de la base (`MEMORIA_BD`, por defecto `~/.memoria/memoria.db`).
    pub bd: PathBuf,
    /// Dónde escucha el servicio (`MEMORIA_DIRECCION`); siempre local.
    pub direccion: SocketAddr,
    /// Carpeta propia (`MEMORIA_DIR`, por defecto `~/.memoria`): base, clave y respaldos.
    pub dir: PathBuf,
    /// Carpeta de registros (`MEMORIA_REGISTROS`, por defecto `~/Library/Logs/memoria`).
    pub registros: PathBuf,
    /// Días de respaldos diarios que se guardan.
    pub dias_respaldo: u32,
    /// Cómo se llama quien usa la memoria (`MEMORIA_NOMBRE`): así queda
    /// nombrado en lo que se guarda de sus conversaciones. Por defecto, «Usuario».
    pub nombre: String,
    /// Quién calcula las huellas (`MEMORIA_HUELLAS`: `interno`, que va por
    /// defecto, u `ollama`).
    pub huellas: Proveedor,
    /// Modelo de huellas (`MEMORIA_MODELO_HUELLAS`). Adentro: `gemma` (por
    /// defecto), `e5-small` o `bge-m3`. Con Ollama, el nombre que tenga allá.
    pub modelo_huellas: String,
    /// Dónde atiende Ollama (`MEMORIA_OLLAMA`), si se usa.
    pub ollama: String,
    /// Carpeta donde se guarda el modelo interno (`MEMORIA_MODELOS`, por
    /// defecto `~/.memoria/modelos`).
    pub modelos: PathBuf,
    /// Cuántos núcleos usa el modelo interno (`MEMORIA_HILOS`; por defecto,
    /// la mitad de los que haya, hasta 8).
    pub hilos: usize,
    /// Umbrales de parecido puestos a mano (`MEMORIA_UMBRALES`, cuatro
    /// números: unión, aviso, piso de notas y piso de conversaciones). Sin
    /// esto van los del modelo.
    pub umbrales: Option<Umbrales>,
    /// Carpeta donde Claude Code guarda las conversaciones
    /// (`MEMORIA_CONVERSACIONES`, por defecto `~/.claude/projects`).
    pub conversaciones: PathBuf,
    /// Las fuentes encendidas (`MEMORIA_FUENTES`, separadas por coma; hoy
    /// solo `whatsapp`). Por defecto, ninguna.
    pub fuentes: Fuentes,
}

const DIRECCION: &str = "127.0.0.1:7337";
const DIAS_RESPALDO: u32 = 14;
const OLLAMA: &str = "http://127.0.0.1:11434";
const MODELO_INTERNO: &str = "gemma";
const MODELO_DE_OLLAMA: &str = "bge-m3";
const HILOS_MAXIMOS: usize = 8;
const BASE_DE_WHATSAPP: &str = "Library/Group Containers/group.net.whatsapp.WhatsApp.shared/ChatStorage.sqlite";

/// Lee `MEMORIA_FUENTES` («whatsapp», o «ninguna») y arma cada fuente
/// encendida con lo suyo: dónde está la base de WhatsApp (`base`, o la de
/// siempre dentro de `casa`) y qué chats quedan afuera (`sin`).
fn leer_fuentes(texto: Option<&str>, casa: &Path, base: Option<&str>, sin: Option<&str>) -> anyhow::Result<Fuentes> {
    let mut fuentes = Fuentes::default();
    for nombre in texto.unwrap_or_default().split(',').map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()) {
        match nombre.as_str() {
            "ninguna" => return Ok(Fuentes::default()),
            "whatsapp" => {
                fuentes.whatsapp = Some(WhatsApp {
                    base: base.map_or_else(|| casa.join(BASE_DE_WHATSAPP), PathBuf::from),
                    sin: sin
                        .unwrap_or_default()
                        .split(';')
                        .map(|chat| chat.trim().to_owned())
                        .filter(|chat| !chat.is_empty())
                        .collect(),
                });
            }
            otra => bail!("MEMORIA_FUENTES: no conozco la fuente «{otra}»; hoy la única es «whatsapp»"),
        }
    }
    Ok(fuentes)
}

/// Lee el archivo de ajustes (`<carpeta>/ajustes`): renglones `NOMBRE=valor`
/// con las mismas variables `MEMORIA_…` que se pueden poner en el entorno.
/// Sirve para elegir, por ejemplo, el modelo de huellas sin tocar los
/// archivos de arranque. Lo que esté puesto en el entorno gana.
fn leer_ajustes(archivo: &Path) -> HashMap<String, String> {
    let Ok(texto) = std::fs::read_to_string(archivo) else {
        return HashMap::new();
    };
    texto
        .lines()
        .map(str::trim)
        .filter(|renglon| !renglon.starts_with('#'))
        .filter_map(|renglon| renglon.split_once('='))
        .map(|(nombre, valor)| (nombre.trim().to_owned(), valor.trim().trim_matches('"').to_owned()))
        .filter(|(nombre, valor)| nombre.starts_with("MEMORIA_") && !valor.is_empty())
        .collect()
}

/// Lee `MEMORIA_UMBRALES`: «0.90, 0.80, 0.45, 0.50».
fn leer_umbrales(texto: &str) -> anyhow::Result<Umbrales> {
    let numeros: Vec<f64> = texto
        .split(',')
        .map(|n| n.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .context("MEMORIA_UMBRALES tiene algo que no es un número")?;
    let [union, aviso, piso_notas, piso_fragmentos] = numeros[..] else {
        bail!("MEMORIA_UMBRALES lleva cuatro números: unión, aviso, piso de notas y piso de conversaciones");
    };
    if numeros.iter().any(|n| !(0.0..=1.0).contains(n)) || union < aviso {
        bail!("MEMORIA_UMBRALES: los cuatro van entre 0 y 1, y la unión no puede pedir menos que el aviso");
    }
    Ok(Umbrales { union, aviso, piso_notas, piso_fragmentos })
}

impl Config {
    pub fn desde_entorno() -> anyhow::Result<Self> {
        let casa = std::env::var_os("HOME").map(PathBuf::from).context("falta la variable HOME")?;
        let del_entorno = |nombre: &str| std::env::var(nombre).ok().filter(|v| !v.trim().is_empty());
        let dir = del_entorno("MEMORIA_DIR").map_or_else(|| casa.join(".memoria"), PathBuf::from);
        let ajustes = leer_ajustes(&dir.join("ajustes"));
        let var = |nombre: &str| del_entorno(nombre).or_else(|| ajustes.get(nombre).cloned());

        let direccion: SocketAddr = var("MEMORIA_DIRECCION")
            .as_deref()
            .unwrap_or(DIRECCION)
            .parse()
            .context("MEMORIA_DIRECCION no es una dirección válida")?;
        if !direccion.ip().is_loopback() {
            bail!("el servicio solo escucha en esta máquina (127.0.0.1), no en {direccion}");
        }
        let huellas = match var("MEMORIA_HUELLAS").as_deref().map(str::trim) {
            None | Some("interno") => Proveedor::Interno,
            Some("ollama") => Proveedor::Ollama,
            Some(otro) => bail!("MEMORIA_HUELLAS es «interno» u «ollama», no «{otro}»"),
        };
        let modelo_por_defecto = match huellas {
            Proveedor::Interno => MODELO_INTERNO,
            Proveedor::Ollama => MODELO_DE_OLLAMA,
        };
        let nucleos = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
        let hilos = match var("MEMORIA_HILOS") {
            Some(h) => h.trim().parse::<usize>().ok().filter(|h| *h > 0).context("MEMORIA_HILOS no es un número")?,
            None => (nucleos / 2).clamp(1, HILOS_MAXIMOS),
        };

        Ok(Self {
            bd: var("MEMORIA_BD").map_or_else(|| dir.join("memoria.db"), PathBuf::from),
            direccion,
            registros: var("MEMORIA_REGISTROS").map_or_else(|| casa.join("Library/Logs/memoria"), PathBuf::from),
            dias_respaldo: DIAS_RESPALDO,
            nombre: var("MEMORIA_NOMBRE").map_or_else(|| NOMBRE_POR_DEFECTO.to_owned(), |n| n.trim().to_owned()),
            huellas,
            modelo_huellas: var("MEMORIA_MODELO_HUELLAS").unwrap_or_else(|| modelo_por_defecto.to_owned()),
            ollama: var("MEMORIA_OLLAMA").map_or_else(|| OLLAMA.to_owned(), |o| o.trim_end_matches('/').to_owned()),
            modelos: var("MEMORIA_MODELOS").map_or_else(|| dir.join("modelos"), PathBuf::from),
            hilos,
            umbrales: var("MEMORIA_UMBRALES").as_deref().map(leer_umbrales).transpose()?,
            conversaciones: var("MEMORIA_CONVERSACIONES").map_or_else(|| casa.join(".claude/projects"), PathBuf::from),
            fuentes: leer_fuentes(
                var("MEMORIA_FUENTES").as_deref(),
                &casa,
                var("MEMORIA_WHATSAPP").as_deref(),
                var("MEMORIA_WHATSAPP_SIN").as_deref(),
            )?,
            dir,
        })
    }

    #[must_use]
    pub fn archivo_clave(&self) -> PathBuf {
        self.dir.join("clave")
    }

    #[must_use]
    pub fn respaldos(&self) -> PathBuf {
        self.dir.join("respaldos")
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn lee_el_archivo_de_ajustes() {
        let archivo = std::env::temp_dir().join(format!("memoria-ajustes-{}", std::process::id()));
        std::fs::write(
            &archivo,
            "# cómo se calculan las huellas\nMEMORIA_HUELLAS = ollama\nMEMORIA_MODELO_HUELLAS=\"bge-m3\"\nOTRA=x\nMEMORIA_HILOS=\nsin signo igual\n",
        )
        .unwrap();
        let ajustes = leer_ajustes(&archivo);
        std::fs::remove_file(&archivo).unwrap();
        assert_eq!(ajustes.get("MEMORIA_HUELLAS").map(String::as_str), Some("ollama"));
        assert_eq!(ajustes.get("MEMORIA_MODELO_HUELLAS").map(String::as_str), Some("bge-m3"));
        assert_eq!(ajustes.len(), 2, "solo las variables de la memoria que traen valor: {ajustes:?}");
        assert!(leer_ajustes(Path::new("/no/existe")).is_empty());
    }

    #[test]
    fn las_fuentes_se_encienden_una_por_una() {
        let casa = Path::new("/Users/ana");
        assert_eq!(leer_fuentes(None, casa, None, None).unwrap(), Fuentes::default());
        assert!(!Fuentes::default().alguna());
        let encendida = leer_fuentes(Some(" WhatsApp "), casa, None, Some("Familia; Los del colegio ;;")).unwrap();
        let whatsapp = encendida.whatsapp.clone().unwrap();
        assert!(encendida.alguna());
        assert!(whatsapp.base.starts_with("/Users/ana/Library/Group Containers"), "{}", whatsapp.base.display());
        assert_eq!(whatsapp.sin, vec!["Familia", "Los del colegio"]);
        let en_otro_lado = leer_fuentes(Some("whatsapp"), casa, Some("/tmp/copia.sqlite"), None).unwrap();
        assert_eq!(en_otro_lado.whatsapp.unwrap().base, Path::new("/tmp/copia.sqlite"));
        // «ninguna» las apaga todas, y una fuente desconocida no pasa de largo.
        assert_eq!(leer_fuentes(Some("whatsapp, ninguna"), casa, None, None).unwrap(), Fuentes::default());
        assert!(leer_fuentes(Some("telegrama"), casa, None, None).is_err());
    }

    #[test]
    fn lee_los_umbrales_puestos_a_mano() {
        let u = leer_umbrales("0.9, 0.8,0.45 ,0.5").unwrap();
        assert!((u.union - 0.9).abs() < 1e-9 && (u.piso_fragmentos - 0.5).abs() < 1e-9);
        for malo in ["0.9,0.8,0.45", "0.9,0.8,0.45,x", "0.7,0.8,0.45,0.5", "1.9,0.8,0.45,0.5"] {
            assert!(leer_umbrales(malo).is_err(), "{malo}");
        }
    }
}
