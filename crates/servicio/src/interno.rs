//! Huellas calculadas dentro del programa, con un modelo que se baja una
//! vez y después corre solo, sin internet y sin ningún otro servicio.
//!
//! Hay tres modelos para elegir (`MEMORIA_MODELO_HUELLAS`). Cada uno mide el
//! parecido en su propia escala, así que trae sus propios umbrales: salen de
//! medirlos con las mismas notas y preguntas (ver `docs/diseno.md`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use tokio::sync::Mutex;

use crate::estado::Umbrales;
use crate::huellas::{Falla, Futuro, Huellas, Uso};

/// Después de este rato sin usarse, el modelo se suelta para liberar memoria;
/// vuelve a cargarse solo (uno o dos segundos) cuando hace falta.
const DESCANSO: Duration = Duration::from_mins(15);

/// Lo que hay que saber de cada modelo.
pub struct Ficha {
    /// El nombre con que se elige.
    pub nombre: &'static str,
    /// De dónde sale y cuánto pesa, para contarlo al instalar.
    pub descripcion: &'static str,
    modelo: EmbeddingModel,
    /// Lo que cada modelo espera delante de una pregunta y delante de un
    /// texto guardado: así sabe qué es qué.
    antes_de_consulta: &'static str,
    antes_de_texto: &'static str,
    pub umbrales: Umbrales,
}

/// Los modelos que se pueden usar. El primero es el que va si no se elige.
pub const FICHAS: [Ficha; 3] = [
    Ficha {
        nombre: "gemma",
        descripcion: "EmbeddingGemma 300M de Google, comprimido a 4 bits (192 MB; licencia Gemma)",
        // De las tres formas en que viene este modelo, esta es la que menos
        // memoria ocupa al trabajar (cerca de 1 GB; la de 8 bits, 3,5 GB).
        modelo: EmbeddingModel::EmbeddingGemma300MQ4,
        antes_de_consulta: "task: search result | query: ",
        antes_de_texto: "title: none | text: ",
        // Una pregunta y su nota dan entre 0,28 y 0,66; una pregunta que no
        // tiene nada que ver, hasta 0,21; la misma nota con otras palabras,
        // 0,88 o más.
        umbrales: Umbrales { union: 0.87, aviso: 0.75, piso_notas: 0.25, piso_fragmentos: 0.30 },
    },
    Ficha {
        nombre: "e5-small",
        descripcion: "multilingual-e5-small (465 MB; licencia MIT)",
        modelo: EmbeddingModel::MultilingualE5Small,
        antes_de_consulta: "query: ",
        antes_de_texto: "passage: ",
        // Este modelo aprieta todos los parecidos entre 0,80 y 0,93: ordena
        // bien, pero distingue peor lo que no tiene nada que ver. Por eso el
        // piso va alto y algunas preguntas dichas con otras palabras se le escapan.
        umbrales: Umbrales { union: 0.965, aviso: 0.93, piso_notas: 0.845, piso_fragmentos: 0.86 },
    },
    Ficha {
        nombre: "bge-m3",
        descripcion: "bge-m3 (2,2 GB; licencia MIT; el de más calidad y el más pesado)",
        modelo: EmbeddingModel::BGEM3,
        antes_de_consulta: "",
        antes_de_texto: "",
        umbrales: Umbrales { union: 0.90, aviso: 0.80, piso_notas: 0.45, piso_fragmentos: 0.50 },
    },
];

/// La ficha de un modelo por su nombre.
#[must_use]
pub fn ficha(nombre: &str) -> Option<&'static Ficha> {
    FICHAS.iter().find(|f| f.nombre.eq_ignore_ascii_case(nombre.trim()))
}

struct Motor {
    modelo: TextEmbedding,
    usado: Instant,
}

/// Un modelo de huellas corriendo adentro del servicio.
pub struct Interno {
    ficha: &'static Ficha,
    /// Como queda anotado en la base con qué se calcularon las huellas.
    identidad: String,
    /// Donde se guardan los archivos del modelo.
    carpeta: PathBuf,
    /// Cuántos núcleos del procesador usa.
    hilos: usize,
    /// El modelo cargado, o `None` mientras descansa. De a un pedido por vez.
    motor: Arc<Mutex<Option<Motor>>>,
}

fn cargar(ficha: &Ficha, carpeta: &Path, hilos: usize, con_avance: bool) -> Result<TextEmbedding, Falla> {
    let opciones = TextInitOptions::new(ficha.modelo.clone())
        .with_cache_dir(carpeta.to_owned())
        .with_show_download_progress(con_avance)
        .with_intra_threads(hilos.max(1));
    TextEmbedding::try_new(opciones).map_err(|e| {
        Falla::Modelo(format!("no pude cargar el modelo {} desde {}: {e}", ficha.nombre, carpeta.display()))
    })
}

impl Interno {
    pub fn nuevo(nombre: &str, carpeta: &Path, hilos: usize) -> Result<Self, Falla> {
        let Some(ficha) = ficha(nombre) else {
            let conocidos: Vec<&str> = FICHAS.iter().map(|f| f.nombre).collect();
            return Err(Falla::Modelo(format!(
                "no conozco el modelo «{nombre}»; los que hay son: {}",
                conocidos.join(", ")
            )));
        };
        Ok(Self {
            ficha,
            identidad: format!("interno/{}", ficha.nombre),
            carpeta: carpeta.to_owned(),
            hilos,
            motor: Arc::new(Mutex::new(None)),
        })
    }

    #[must_use]
    pub fn ficha(&self) -> &'static Ficha {
        self.ficha
    }

    /// Si los archivos del modelo ya están bajados en la carpeta.
    #[must_use]
    pub fn bajado(&self) -> bool {
        std::fs::read_dir(&self.carpeta)
            .is_ok_and(|entradas| entradas.flatten().any(|e| e.file_name().to_string_lossy().starts_with("models--")))
            && self.archivos_del_modelo()
    }

    /// Busca, sin cargarlo, si está el archivo principal de este modelo.
    fn archivos_del_modelo(&self) -> bool {
        let Ok(info) = TextEmbedding::get_model_info(&self.ficha.modelo) else { return false };
        let repo = format!("models--{}", info.model_code.replace('/', "--"));
        let fotos = self.carpeta.join(repo).join("snapshots");
        std::fs::read_dir(fotos)
            .is_ok_and(|entradas| entradas.flatten().any(|e| e.path().join(&info.model_file).is_file()))
    }

    /// Deja el modelo listo: lo baja si falta, lo carga y lo prueba. Con
    /// `con_avance` muestra por pantalla cómo va la bajada. Devuelve cuánto
    /// tardó en calcular una huella una vez cargado.
    pub async fn preparar(&self, con_avance: bool) -> Result<Duration, Falla> {
        let mut guardia = Arc::clone(&self.motor).lock_owned().await;
        let (ficha, carpeta, hilos) = (self.ficha, self.carpeta.clone(), self.hilos);
        tokio::task::spawn_blocking(move || {
            let mut modelo = cargar(ficha, &carpeta, hilos, con_avance)?;
            let prueba = vec![format!("{}hola", ficha.antes_de_consulta)];
            modelo.embed(prueba.clone(), None).map_err(|e| Falla::Modelo(e.to_string()))?;
            let inicio = Instant::now();
            modelo.embed(prueba, None).map_err(|e| Falla::Modelo(e.to_string()))?;
            let tardo = inicio.elapsed();
            *guardia = Some(Motor { modelo, usado: Instant::now() });
            Ok(tardo)
        })
        .await
        .map_err(|e| Falla::Modelo(e.to_string()))?
    }

    /// Calcula una huella. El turno se pide para cada texto: una consulta que
    /// llega mientras se completan huellas pendientes espera, como mucho, un texto.
    async fn una(&self, entrada: String) -> Result<Vec<f32>, Falla> {
        let mut guardia = Arc::clone(&self.motor).lock_owned().await;
        let (ficha, carpeta, hilos) = (self.ficha, self.carpeta.clone(), self.hilos);
        tokio::task::spawn_blocking(move || {
            if guardia.is_none() {
                let inicio = Instant::now();
                let modelo = cargar(ficha, &carpeta, hilos, false)?;
                tracing::info!(modelo = ficha.nombre, ms = inicio.elapsed().as_millis(), "modelo de huellas cargado");
                *guardia = Some(Motor { modelo, usado: Instant::now() });
            }
            let Some(motor) = guardia.as_mut() else {
                return Err(Falla::Modelo("el modelo no quedó cargado".to_owned()));
            };
            motor.usado = Instant::now();
            // De a un texto: en los modelos comprimidos la huella de un
            // texto cambia según con cuáles otros se calcule.
            let mut huellas = motor.modelo.embed(vec![entrada], None).map_err(|e| Falla::Modelo(e.to_string()))?;
            huellas.pop().ok_or_else(|| Falla::Modelo("el modelo no devolvió ninguna huella".to_owned()))
        })
        .await
        .map_err(|e| Falla::Modelo(e.to_string()))?
    }
}

impl Huellas for Interno {
    fn modelo(&self) -> &str {
        &self.identidad
    }

    fn umbrales(&self) -> Umbrales {
        self.ficha.umbrales
    }

    fn calcular<'a>(&'a self, textos: &'a [String], uso: Uso) -> Futuro<'a, Result<Vec<Vec<f32>>, Falla>> {
        Box::pin(async move {
            let antes = match uso {
                Uso::Consulta => self.ficha.antes_de_consulta,
                Uso::Texto => self.ficha.antes_de_texto,
            };
            let mut huellas = Vec::with_capacity(textos.len());
            for texto in textos {
                huellas.push(self.una(format!("{antes}{texto}")).await?);
            }
            Ok(huellas)
        })
    }

    fn descansar(&self) {
        // Si está ocupado, no descansa: se mira de nuevo en la próxima vuelta.
        if let Ok(mut guardia) = self.motor.try_lock()
            && guardia.as_ref().is_some_and(|motor| motor.usado.elapsed() >= DESCANSO)
        {
            *guardia = None;
            tracing::info!(modelo = self.ficha.nombre, "solté el modelo de huellas: llevaba un rato sin usarse");
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn conoce_sus_modelos() {
        assert_eq!(ficha("gemma").map(|f| f.nombre), Some("gemma"));
        assert_eq!(ficha(" E5-Small ").map(|f| f.nombre), Some("e5-small"));
        assert!(ficha("otro").is_none());
        let falla = Interno::nuevo("otro", Path::new("/tmp"), 2).err().map(|f| f.to_string()).unwrap_or_default();
        assert!(falla.contains("gemma, e5-small, bge-m3"), "{falla}");
        let interno = Interno::nuevo("gemma", Path::new("/tmp/no-existe"), 2).unwrap();
        assert_eq!(interno.modelo(), "interno/gemma");
        assert!(!interno.bajado());
        // Cada modelo trae umbrales en orden: unir pide más parecido que avisar.
        for f in &FICHAS {
            assert!(
                f.umbrales.union > f.umbrales.aviso && f.umbrales.aviso > f.umbrales.piso_fragmentos,
                "{}",
                f.nombre
            );
            assert!(f.umbrales.piso_fragmentos >= f.umbrales.piso_notas, "{}", f.nombre);
        }
    }
}
