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
}

const DIRECCION: &str = "127.0.0.1:7337";
const DIAS_RESPALDO: u32 = 14;
const OLLAMA: &str = "http://127.0.0.1:11434";
const MODELO_INTERNO: &str = "gemma";
const MODELO_DE_OLLAMA: &str = "bge-m3";
const HILOS_MAXIMOS: usize = 8;

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
    fn lee_los_umbrales_puestos_a_mano() {
        let u = leer_umbrales("0.9, 0.8,0.45 ,0.5").unwrap();
        assert!((u.union - 0.9).abs() < 1e-9 && (u.piso_fragmentos - 0.5).abs() < 1e-9);
        for malo in ["0.9,0.8,0.45", "0.9,0.8,0.45,x", "0.7,0.8,0.45,0.5", "1.9,0.8,0.45,0.5"] {
            assert!(leer_umbrales(malo).is_err(), "{malo}");
        }
    }
}
