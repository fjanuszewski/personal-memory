//! Configuración del servicio, leída del entorno con valores por defecto
//! pensados para una Mac.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// De dónde sale la llave de un espacio de Slack.
#[derive(Clone, PartialEq, Eq)]
pub enum Llave {
    /// Del llavero de macOS: el nombre de la entrada donde está guardada.
    Llavero(String),
    /// No hay llave: el espacio se lee por la conexión de Slack que el
    /// usuario tiene en Claude, y lo trae el mod de una conversación abierta.
    Conexion,
    /// Puesta en el código. Solo para las pruebas: una llave de verdad no se
    /// escribe en ningún archivo.
    Fija(String),
}

impl std::fmt::Debug for Llave {
    /// Nunca muestra una llave: ni en un registro ni en un error.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Llavero(entrada) => write!(f, "Llavero({entrada:?})"),
            Self::Conexion => f.write_str("Conexion"),
            Self::Fija(_) => f.write_str("Fija(…)"),
        }
    }
}

/// Un espacio de trabajo de Slack del que se lee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Espacio {
    /// Cómo lo llama el usuario: así queda nombrado en lo que se guarda.
    pub nombre: String,
    pub llave: Llave,
}

/// Slack como fuente para aprender cómo escribe el usuario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slack {
    /// Los espacios de trabajo (`MEMORIA_SLACK`, separados por punto y
    /// coma): «nombre=entrada del llavero», o «nombre=conexion» si se lee
    /// por la conexión de Slack de Claude.
    pub espacios: Vec<Espacio>,
    /// Canales y personas que quedan afuera: los que tienen alguno de estos
    /// textos en el nombre (`MEMORIA_SLACK_SIN`, separados por punto y coma).
    pub sin: Vec<String>,
    /// Dónde atiende Slack: siempre el de verdad, salvo en las pruebas.
    pub api: String,
    /// Cuánto se espera entre una búsqueda y la siguiente, para no pasarse
    /// del ritmo que permite Slack.
    pub pausa: Duration,
}

/// De dónde más, aparte de las conversaciones con Claude, se aprende cómo
/// escribe el usuario. Cada fuente se enciende aparte; apagada, no se lee.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fuentes {
    pub whatsapp: Option<WhatsApp>,
    pub slack: Option<Slack>,
}

impl Fuentes {
    /// Si hay alguna fuente encendida.
    #[must_use]
    pub const fn alguna(&self) -> bool {
        self.whatsapp.is_some() || self.slack.is_some()
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
    /// Las fuentes encendidas (`MEMORIA_FUENTES`, separadas por coma:
    /// `whatsapp`, `slack`). Por defecto, ninguna.
    pub fuentes: Fuentes,
}

const DIRECCION: &str = "127.0.0.1:7337";
const DIAS_RESPALDO: u32 = 14;
const OLLAMA: &str = "http://127.0.0.1:11434";
const MODELO_INTERNO: &str = "gemma";
const MODELO_DE_OLLAMA: &str = "bge-m3";
const HILOS_MAXIMOS: usize = 8;
const BASE_DE_WHATSAPP: &str = "Library/Group Containers/group.net.whatsapp.WhatsApp.shared/ChatStorage.sqlite";
/// Dónde atiende Slack.
pub const API_DE_SLACK: &str = "https://slack.com/api";
/// Slack deja hacer unas veinte búsquedas por minuto.
const PAUSA_DE_SLACK: Duration = Duration::from_secs(3);

/// Una lista separada por punto y coma, sin los lugares vacíos.
fn lista(texto: Option<&str>) -> Vec<String> {
    texto
        .unwrap_or_default()
        .split(';')
        .map(|parte| parte.trim().to_owned())
        .filter(|parte| !parte.is_empty())
        .collect()
}

/// Lee `MEMORIA_SLACK`: de cada espacio de trabajo, el nombre que le pone el
/// usuario y en qué entrada del llavero está su llave («acme=slack-acme;
/// globex=slack-globex»). En vez de la entrada puede ir «conexion»: ese
/// espacio no tiene llave y se lee por la conexión de Slack de Claude.
fn leer_espacios(texto: Option<&str>) -> anyhow::Result<Vec<Espacio>> {
    let mut espacios: Vec<Espacio> = Vec::new();
    for parte in lista(texto) {
        let (nombre, entrada) = match parte.split_once('=') {
            Some((nombre, entrada)) if !nombre.trim().is_empty() && !entrada.trim().is_empty() => {
                (nombre.trim().to_owned(), entrada.trim().to_owned())
            }
            _ => bail!("MEMORIA_SLACK: «{parte}» tiene que ser «nombre=entrada del llavero» o «nombre=conexion»"),
        };
        if entrada.starts_with("xox") {
            bail!(
                "MEMORIA_SLACK: en «{nombre}» va el nombre de la entrada del llavero donde está la llave, no la \
                 llave: una llave no se escribe en ningún archivo"
            );
        }
        if espacios.iter().any(|espacio| espacio.nombre.eq_ignore_ascii_case(&nombre)) {
            bail!("MEMORIA_SLACK: el espacio «{nombre}» está dos veces");
        }
        let llave = match entrada.to_lowercase().as_str() {
            "conexion" | "conexión" => Llave::Conexion,
            _ => Llave::Llavero(entrada),
        };
        espacios.push(Espacio { nombre, llave });
    }
    if espacios.is_empty() {
        bail!(
            "MEMORIA_FUENTES enciende Slack, pero falta MEMORIA_SLACK: de cada espacio de trabajo, un nombre y la \
             entrada del llavero donde está su llave («acme=slack-acme»)"
        );
    }
    Ok(espacios)
}

/// Lee `MEMORIA_FUENTES` («whatsapp», «slack», o «ninguna») y arma cada
/// fuente encendida con lo suyo, que saca de `var`: de WhatsApp, dónde está
/// la base (`MEMORIA_WHATSAPP`, o la de siempre dentro de `casa`) y qué chats
/// quedan afuera (`MEMORIA_WHATSAPP_SIN`); de Slack, los espacios de trabajo
/// (`MEMORIA_SLACK`) y qué canales y personas quedan afuera
/// (`MEMORIA_SLACK_SIN`).
fn leer_fuentes(texto: Option<&str>, casa: &Path, var: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Fuentes> {
    let mut fuentes = Fuentes::default();
    for nombre in texto.unwrap_or_default().split(',').map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()) {
        match nombre.as_str() {
            "ninguna" => return Ok(Fuentes::default()),
            "whatsapp" => {
                fuentes.whatsapp = Some(WhatsApp {
                    base: var("MEMORIA_WHATSAPP").map_or_else(|| casa.join(BASE_DE_WHATSAPP), PathBuf::from),
                    sin: lista(var("MEMORIA_WHATSAPP_SIN").as_deref()),
                });
            }
            "slack" => {
                fuentes.slack = Some(Slack {
                    espacios: leer_espacios(var("MEMORIA_SLACK").as_deref())?,
                    sin: lista(var("MEMORIA_SLACK_SIN").as_deref()),
                    api: API_DE_SLACK.to_owned(),
                    pausa: PAUSA_DE_SLACK,
                });
            }
            otra => bail!("MEMORIA_FUENTES: no conozco la fuente «{otra}»; las que hay son «whatsapp» y «slack»"),
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
            fuentes: leer_fuentes(var("MEMORIA_FUENTES").as_deref(), &casa, &var)?,
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

    /// Lee las fuentes con estos ajustes puestos, como si vinieran del entorno.
    fn fuentes_con(texto: Option<&str>, puestos: &[(&str, &str)]) -> anyhow::Result<Fuentes> {
        let var = |nombre: &str| puestos.iter().find(|(n, _)| *n == nombre).map(|(_, valor)| (*valor).to_owned());
        leer_fuentes(texto, Path::new("/Users/ana"), &var)
    }

    #[test]
    fn las_fuentes_se_encienden_una_por_una() {
        assert_eq!(fuentes_con(None, &[]).unwrap(), Fuentes::default());
        assert!(!Fuentes::default().alguna());
        let encendida =
            fuentes_con(Some(" WhatsApp "), &[("MEMORIA_WHATSAPP_SIN", "Familia; Los del colegio ;;")]).unwrap();
        let whatsapp = encendida.whatsapp.clone().unwrap();
        assert!(encendida.alguna() && encendida.slack.is_none());
        assert!(whatsapp.base.starts_with("/Users/ana/Library/Group Containers"), "{}", whatsapp.base.display());
        assert_eq!(whatsapp.sin, vec!["Familia", "Los del colegio"]);
        let en_otro_lado = fuentes_con(Some("whatsapp"), &[("MEMORIA_WHATSAPP", "/tmp/copia.sqlite")]).unwrap();
        assert_eq!(en_otro_lado.whatsapp.unwrap().base, Path::new("/tmp/copia.sqlite"));
        // «ninguna» las apaga todas, y una fuente desconocida no pasa de largo.
        assert_eq!(fuentes_con(Some("whatsapp, ninguna"), &[]).unwrap(), Fuentes::default());
        assert!(fuentes_con(Some("telegrama"), &[]).is_err());
    }

    #[test]
    fn slack_se_enciende_con_sus_espacios_y_sin_ninguna_llave_a_la_vista() {
        let puestos = [("MEMORIA_SLACK", " acme = slack-acme ; globex=slack-globex;"), ("MEMORIA_SLACK_SIN", "random")];
        let encendidas = fuentes_con(Some("whatsapp, Slack"), &puestos).unwrap();
        assert!(encendidas.whatsapp.is_some());
        let slack = encendidas.slack.unwrap();
        assert_eq!(
            slack.espacios,
            vec![
                Espacio { nombre: "acme".into(), llave: Llave::Llavero("slack-acme".into()) },
                Espacio { nombre: "globex".into(), llave: Llave::Llavero("slack-globex".into()) },
            ]
        );
        assert_eq!((slack.api.as_str(), slack.sin), (API_DE_SLACK, vec!["random".to_owned()]));
        // Sin espacios no hay de dónde leer, y cada uno va con su entrada del llavero.
        assert!(fuentes_con(Some("slack"), &[]).is_err());
        for malo in ["acme", "acme=", "=slack-acme", "acme=slack-acme;ACME=otra"] {
            assert!(fuentes_con(Some("slack"), &[("MEMORIA_SLACK", malo)]).is_err(), "{malo}");
        }
        // Una llave pegada en los ajustes no se acepta, y el error no la repite.
        let pegada = fuentes_con(Some("slack"), &[("MEMORIA_SLACK", "acme=xoxp-una-llave-de-mentira")]).unwrap_err();
        assert!(pegada.to_string().contains("no la llave") && !pegada.to_string().contains("xoxp"), "{pegada}");
        // Una llave nunca se muestra.
        assert_eq!(format!("{:?}", Llave::Fija("xoxp-una-llave-de-mentira".into())), "Fija(…)");
        // Un espacio sin llave se lee por la conexión de Slack de Claude.
        let mezcla = fuentes_con(Some("slack"), &[("MEMORIA_SLACK", "acme=slack-acme; globex = Conexión")]).unwrap();
        let llaves: Vec<Llave> = mezcla.slack.unwrap().espacios.into_iter().map(|espacio| espacio.llave).collect();
        assert_eq!(llaves, vec![Llave::Llavero("slack-acme".into()), Llave::Conexion]);
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
