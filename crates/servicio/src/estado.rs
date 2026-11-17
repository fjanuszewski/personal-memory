//! Lo que comparten todas las partes del servicio: la base, las huellas en
//! memoria, quien las calcula y los umbrales de calidad.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::Notify;

use crate::config::Fuentes;
use crate::huellas::{Huellas, Uso};
use crate::indice::Indice;

/// Los números que deciden cuándo dos textos «dicen lo mismo». Cada modelo
/// de huellas mide en su propia escala y trae los suyos (`interno.rs`). Los
/// que van acá son los de `bge-m3`: salen de medirlo con notas y preguntas
/// de ejemplo en español (una pregunta y su nota dan entre 0,45 y 0,70; una
/// pregunta que no tiene nada que ver, hasta 0,44; la misma nota con otras
/// palabras, 0,90 o más).
#[derive(Clone, Copy, Debug)]
pub struct Umbrales {
    /// Desde este parecido, dos notas del mismo grupo y tipo se unen.
    pub union: f64,
    /// Desde este parecido, la nota se guarda pero se avisa cuál se le parece.
    pub aviso: f64,
    /// Parecido mínimo para que una nota aparezca por significado.
    pub piso_notas: f64,
    /// Parecido mínimo para un pedazo de conversación. Es más alto porque hay
    /// muchos más pedazos que notas y alguno siempre queda cerca por azar.
    pub piso_fragmentos: f64,
}

impl Default for Umbrales {
    fn default() -> Self {
        Self { union: 0.90, aviso: 0.80, piso_notas: 0.45, piso_fragmentos: 0.50 }
    }
}

/// Cómo se nombra a quien usa la memoria si no eligió otro nombre.
pub const NOMBRE_POR_DEFECTO: &str = "Usuario";

const SIN_PROBAR: u8 = 0;
const ANDAN: u8 = 1;
const NO_ANDAN: u8 = 2;

#[derive(Clone)]
pub struct Estado {
    pub pool: SqlitePool,
    pub huellas: Arc<dyn Huellas>,
    /// Las huellas ya calculadas, en memoria, para comparar rápido.
    pub indice: Arc<Indice>,
    pub umbrales: Umbrales,
    /// Cómo se nombra, en lo que se guarda de las conversaciones, a quien usa
    /// la memoria.
    pub nombre: Arc<str>,
    /// Despierta a la tarea que completa las huellas que faltan.
    pub pendientes: Arc<Notify>,
    /// Las fuentes encendidas para aprender cómo escribe el usuario, aparte
    /// de sus conversaciones con Claude.
    pub fuentes: Arc<Fuentes>,
    /// Desde cuándo (segundos desde 1970) el servicio está leyendo
    /// WhatsApp; cero si no lo está leyendo. Esa lectura puede quedar
    /// esperando a que el usuario le conteste a macOS si la deja.
    pub leyendo_whatsapp: Arc<AtomicI64>,
    salud_huellas: Arc<AtomicU8>,
}

impl Estado {
    #[must_use]
    pub fn nuevo(pool: SqlitePool, huellas: Arc<dyn Huellas>) -> Self {
        Self {
            pool,
            umbrales: huellas.umbrales(),
            huellas,
            indice: Arc::new(Indice::default()),
            nombre: Arc::from(NOMBRE_POR_DEFECTO),
            pendientes: Arc::new(Notify::new()),
            fuentes: Arc::new(Fuentes::default()),
            leyendo_whatsapp: Arc::new(AtomicI64::new(0)),
            salud_huellas: Arc::new(AtomicU8::new(SIN_PROBAR)),
        }
    }

    /// Las huellas de varios textos, o `None` si no llegaron a tiempo. La
    /// memoria sigue andando sin ellas (solo por palabras); el cambio se
    /// registra una vez, no en cada pedido.
    pub async fn huellas_de(&self, textos: &[String], uso: Uso, limite: Duration) -> Option<Vec<Vec<f32>>> {
        let resultado = match tokio::time::timeout(limite, self.huellas.calcular(textos, uso)).await {
            Ok(Ok(huellas)) => Ok(huellas),
            Ok(Err(falla)) => Err(falla.to_string()),
            Err(_) => Err(format!("tardó más de {} s", limite.as_secs())),
        };
        match resultado {
            Ok(huellas) => {
                if self.salud_huellas.swap(ANDAN, Ordering::Relaxed) == NO_ANDAN {
                    tracing::info!("las huellas volvieron a andar");
                }
                Some(huellas)
            }
            Err(motivo) => {
                if self.salud_huellas.swap(NO_ANDAN, Ordering::Relaxed) != NO_ANDAN {
                    tracing::warn!(%motivo, "no pude calcular huellas: sigo solo por palabras");
                }
                None
            }
        }
    }

    /// La huella de un solo texto.
    pub async fn huella_de(&self, texto: &str, uso: Uso, limite: Duration) -> Option<Vec<f32>> {
        self.huellas_de(&[texto.to_owned()], uso, limite).await?.into_iter().next()
    }

    /// Si el último pedido de huellas anduvo; `None` si todavía no hubo ninguno.
    #[must_use]
    pub fn huellas_andan(&self) -> Option<bool> {
        match self.salud_huellas.load(Ordering::Relaxed) {
            ANDAN => Some(true),
            NO_ANDAN => Some(false),
            _ => None,
        }
    }
}
