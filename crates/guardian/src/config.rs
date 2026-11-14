//! Configuración del guardián, leída del entorno con valores por defecto
//! pensados para esta Mac.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;

#[derive(Clone, Debug)]
pub struct Config {
    /// El archivo de la base real (`MEMORIA_BD`, por defecto `~/.memoria/memoria.db`).
    pub bd: PathBuf,
    /// La copia del código de la versión instalada (`MEMORIA_REPO`; por
    /// defecto, `codigo` dentro de la carpeta propia). La deja el instalador,
    /// y ahí el guardián arma y guarda cada mejora: nunca toca el
    /// repositorio donde se desarrolla.
    pub repo: PathBuf,
    /// Carpeta propia (`MEMORIA_DIR`, por defecto `~/.memoria`).
    pub dir: PathBuf,
    /// Carpeta de registros (`MEMORIA_REGISTROS`).
    pub registros: PathBuf,
    /// Cómo calcula las huellas el servicio instalado: las variables
    /// `MEMORIA_HUELLAS`, `MEMORIA_MODELO_HUELLAS`, `MEMORIA_OLLAMA`,
    /// `MEMORIA_MODELOS`, `MEMORIA_HILOS` y `MEMORIA_UMBRALES` que estén
    /// puestas. Las copias de prueba del servicio arrancan con las mismas,
    /// para medir con el mismo modelo que se usa de verdad.
    pub huellas: Vec<(&'static str, String)>,
    /// Dónde atiende el servicio instalado (`MEMORIA_SERVICIO`).
    pub servicio: String,
    /// Nombre del agente de launchd del servicio (`MEMORIA_ETIQUETA`).
    pub etiqueta: String,
    /// El programa de Claude Code que hace de agente (`MEMORIA_AGENTE`).
    pub agente: PathBuf,
    /// Cuánto puede trabajar el agente en una mejora.
    pub limite_agente: Duration,
    /// Dónde buscar `cargo`, `git` y demás (`PATH` para lo que se lanza).
    pub ruta: String,
    /// Cuántas mejoras se trabajan a la vez (la instalación es de a una).
    pub a_la_vez: usize,
    /// Primer puerto para las copias del servicio que se prenden al probar.
    pub puerto_pruebas: u16,
    /// Órdenes para reiniciar y apagar el servicio, si no es por launchd
    /// (`MEMORIA_REINICIO` y `MEMORIA_APAGADO`; se usan en los ensayos).
    pub reinicio: Option<String>,
    pub apagado: Option<String>,
    pub casa: PathBuf,
    pub usuario: String,
}

const SERVICIO: &str = "http://127.0.0.1:7337";
const ETIQUETA: &str = "personal-memory.memoria";
const LIMITE_AGENTE: Duration = Duration::from_mins(30);
/// Lo que decide cómo se calculan las huellas (ver la configuración del servicio).
const DE_LAS_HUELLAS: [&str; 5] =
    ["MEMORIA_HUELLAS", "MEMORIA_MODELO_HUELLAS", "MEMORIA_OLLAMA", "MEMORIA_HILOS", "MEMORIA_UMBRALES"];

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

/// El Claude Code más nuevo que haya: el de la app suele estar más al día
/// que el de la terminal.
fn agente_por_defecto(casa: &Path) -> PathBuf {
    let de_la_app = casa.join("Library/Application Support/Claude/claude-code");
    let mut candidatos: Vec<(Vec<u32>, PathBuf)> = Vec::new();
    for version in std::fs::read_dir(&de_la_app).into_iter().flatten().flatten() {
        let numeros: Vec<u32> =
            version.file_name().to_string_lossy().split('.').filter_map(|n| n.parse().ok()).collect();
        for armado in std::fs::read_dir(version.path()).into_iter().flatten().flatten() {
            let programa = armado.path().join("claude.app/Contents/MacOS/claude");
            if programa.is_file() {
                candidatos.push((numeros.clone(), programa));
            }
        }
    }
    candidatos.sort();
    if let Some((_, programa)) = candidatos.pop() {
        return programa;
    }
    let local = casa.join(".claude/local/claude");
    if local.is_file() { local } else { PathBuf::from("claude") }
}

impl Config {
    pub fn desde_entorno() -> anyhow::Result<Self> {
        let casa = std::env::var_os("HOME").map(PathBuf::from).context("falta la variable HOME")?;
        let del_entorno = |nombre: &str| std::env::var(nombre).ok().filter(|v| !v.trim().is_empty());
        let usuario = del_entorno("USER").context("falta la variable USER")?;
        let dir = del_entorno("MEMORIA_DIR").map_or_else(|| casa.join(".memoria"), PathBuf::from);
        let ajustes = leer_ajustes(&dir.join("ajustes"));
        let var = |nombre: &str| del_entorno(nombre).or_else(|| ajustes.get(nombre).cloned());
        let ruta = format!(
            "{}/.cargo/bin:/opt/homebrew/opt/rustup/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            casa.display()
        );
        let mut huellas: Vec<(&'static str, String)> =
            DE_LAS_HUELLAS.iter().filter_map(|nombre| Some((*nombre, var(nombre)?))).collect();
        // El modelo se baja una sola vez: las copias de prueba usan el mismo.
        let modelos = var("MEMORIA_MODELOS").map_or_else(|| dir.join("modelos"), PathBuf::from);
        huellas.push(("MEMORIA_MODELOS", modelos.display().to_string()));
        Ok(Self {
            huellas,
            bd: var("MEMORIA_BD").map_or_else(|| dir.join("memoria.db"), PathBuf::from),
            repo: var("MEMORIA_REPO").map_or_else(|| dir.join("codigo"), PathBuf::from),
            registros: var("MEMORIA_REGISTROS").map_or_else(|| casa.join("Library/Logs/memoria"), PathBuf::from),
            servicio: var("MEMORIA_SERVICIO")
                .map_or_else(|| SERVICIO.to_owned(), |s| s.trim_end_matches('/').to_owned()),
            etiqueta: var("MEMORIA_ETIQUETA").unwrap_or_else(|| ETIQUETA.to_owned()),
            agente: var("MEMORIA_AGENTE").map_or_else(|| agente_por_defecto(&casa), PathBuf::from),
            limite_agente: LIMITE_AGENTE,
            ruta: var("MEMORIA_RUTA").unwrap_or(ruta),
            a_la_vez: 2,
            puerto_pruebas: var("MEMORIA_PUERTO_PRUEBAS").and_then(|p| p.parse().ok()).unwrap_or(7400),
            reinicio: var("MEMORIA_REINICIO"),
            apagado: var("MEMORIA_APAGADO"),
            dir,
            casa,
            usuario,
        })
    }

    /// El programa del servicio que está instalado.
    #[must_use]
    pub fn programa(&self) -> PathBuf {
        self.dir.join("bin/memoria")
    }

    /// Donde se arman las copias de trabajo de cada mejora.
    #[must_use]
    pub fn trabajo(&self) -> PathBuf {
        self.dir.join("trabajo")
    }

    #[must_use]
    pub fn respaldos(&self) -> PathBuf {
        self.dir.join("respaldos")
    }

    /// El archivo del agente de launchd del servicio.
    #[must_use]
    pub fn plist(&self) -> PathBuf {
        self.casa.join(format!("Library/LaunchAgents/{}.plist", self.etiqueta))
    }

    /// De qué versión del código (commit de git) salió el programa instalado.
    /// Lo escribe el instalador y lo actualiza el guardián.
    #[must_use]
    pub fn marca_instalada(&self) -> PathBuf {
        self.dir.join("instalada")
    }

    /// La llave para que el agente use Claude sin la app, si se guardó una.
    #[must_use]
    pub fn llave_agente(&self) -> PathBuf {
        self.dir.join("agente.token")
    }

    /// El entorno mínimo con que se lanza todo: nada de lo que tenga el
    /// guardián pasa sin querer a lo que corre.
    #[must_use]
    pub fn entorno(&self) -> Vec<(&'static str, String)> {
        vec![
            ("HOME", self.casa.display().to_string()),
            ("USER", self.usuario.clone()),
            ("LOGNAME", self.usuario.clone()),
            ("PATH", self.ruta.clone()),
            ("LANG", "es_AR.UTF-8".to_owned()),
            ("TMPDIR", std::env::temp_dir().display().to_string()),
        ]
    }
}
