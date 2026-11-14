//! Huellas de significado: una lista de números por texto, que queda cerca
//! de la de otro texto cuando dicen lo mismo con otras palabras.
//!
//! Hay dos formas de calcularlas, detrás de la misma interfaz: con un modelo
//! que corre dentro del programa (`interno.rs`, lo que va por defecto) o
//! pidiéndoselas a Ollama, para quien ya lo tiene andando con placa gráfica.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::base;
use crate::error::Resultado;
use crate::estado::{Estado, Umbrales};
use crate::indice::{self, Clase, Dato, Indice};

/// El largo de las huellas de `bge-m3`.
pub const DIMENSIONES: usize = 1024;
/// Una huella con menos números que esto no es una huella.
const MINIMO_DE_NUMEROS: usize = 32;

/// Cuánto deja Ollama el modelo cargado después de cada pedido, para que el
/// siguiente no espere a que se cargue de nuevo.
const MANTENER_CARGADO: &str = "30m";

/// Cuántos textos se mandan juntos al completar lo pendiente.
const LOTE: i64 = 16;
const LIMITE_LOTE: Duration = Duration::from_secs(120);
const ESPERA_SIN_TRABAJO: Duration = Duration::from_secs(60);
const ESPERA_CON_FALLA: Duration = Duration::from_secs(30);
/// Cuánto se espera la primera huella al arrancar: puede haber que bajar el modelo.
const LIMITE_PRIMERA: Duration = Duration::from_mins(30);

#[derive(Debug, thiserror::Error)]
pub enum Falla {
    #[error("no pude hablar con Ollama: {0}")]
    Conexion(String),
    #[error("Ollama respondió algo inesperado: {0}")]
    Respuesta(String),
    #[error("el modelo de huellas falló: {0}")]
    Modelo(String),
}

/// Para qué es la huella: algunos modelos tratan distinto una pregunta que
/// un texto guardado.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uso {
    /// Lo que se busca.
    Consulta,
    /// Una nota o un pedazo de conversación.
    Texto,
}

pub type Futuro<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Quien calcula las huellas.
pub trait Huellas: Send + Sync {
    /// El nombre del modelo: si cambia, las huellas guardadas dejan de servir.
    fn modelo(&self) -> &str;

    /// Los umbrales de parecido que le corresponden: cada modelo mide en su
    /// propia escala.
    fn umbrales(&self) -> Umbrales {
        Umbrales::default()
    }

    /// Una huella por cada texto, en el mismo orden y todas del mismo largo.
    fn calcular<'a>(&'a self, textos: &'a [String], uso: Uso) -> Futuro<'a, Result<Vec<Vec<f32>>, Falla>>;

    /// Se llama cada tanto: si hace rato que no se usa, puede soltar lo que
    /// tenga cargado en memoria.
    fn descansar(&self) {}
}

/// El texto de una nota que se convierte en huella.
#[must_use]
pub fn texto_de_nota(titulo: &str, cuerpo: &str) -> String {
    format!("{titulo}\n{cuerpo}")
}

/// Ollama en esta Mac.
pub struct Ollama {
    cliente: reqwest::Client,
    direccion: String,
    modelo: String,
}

#[derive(Serialize)]
struct Pedido<'a> {
    model: &'a str,
    input: &'a [String],
    keep_alive: &'a str,
    truncate: bool,
}

#[derive(Deserialize)]
struct Respuesta {
    embeddings: Vec<Vec<f32>>,
}

impl Ollama {
    pub fn nuevo(direccion: &str, modelo: &str) -> Result<Self, Falla> {
        let cliente = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(LIMITE_LOTE)
            .build()
            .map_err(|e| Falla::Conexion(e.to_string()))?;
        Ok(Self { cliente, direccion: direccion.trim_end_matches('/').to_owned(), modelo: modelo.to_owned() })
    }
}

impl Huellas for Ollama {
    fn modelo(&self) -> &str {
        &self.modelo
    }

    fn calcular<'a>(&'a self, textos: &'a [String], _: Uso) -> Futuro<'a, Result<Vec<Vec<f32>>, Falla>> {
        Box::pin(async move {
            if textos.is_empty() {
                return Ok(Vec::new());
            }
            let pedido = Pedido { model: &self.modelo, input: textos, keep_alive: MANTENER_CARGADO, truncate: true };
            let respuesta = self
                .cliente
                .post(format!("{}/api/embed", self.direccion))
                .json(&pedido)
                .send()
                .await
                .map_err(|e| Falla::Conexion(e.to_string()))?;
            let estado = respuesta.status();
            if !estado.is_success() {
                let cuerpo = respuesta.text().await.unwrap_or_default();
                let cuerpo: String = cuerpo.chars().take(200).collect();
                return Err(Falla::Respuesta(format!("{estado}: {cuerpo}")));
            }
            let Respuesta { embeddings } = respuesta.json().await.map_err(|e| Falla::Respuesta(e.to_string()))?;
            if embeddings.len() != textos.len() {
                return Err(Falla::Respuesta(format!("pedí {} huellas y llegaron {}", textos.len(), embeddings.len())));
            }
            let largo = embeddings.first().map_or(0, Vec::len);
            if let Some(mala) = embeddings.iter().find(|h| h.len() < MINIMO_DE_NUMEROS || h.len() != largo) {
                return Err(Falla::Respuesta(format!(
                    "el modelo {} da huellas de {} números: así no sirven",
                    self.modelo,
                    mala.len()
                )));
            }
            Ok(embeddings)
        })
    }
}

/// Si las huellas guardadas son de otro modelo, las borra para que se
/// calculen de nuevo: las de modelos distintos no se pueden comparar.
pub async fn al_arrancar(pool: &SqlitePool, modelo: &str) -> Resultado<()> {
    let anterior: Option<String> =
        sqlx::query_scalar("select valor from ajustes where clave = 'modelo_huellas'").fetch_optional(pool).await?;
    if anterior.as_deref() == Some(modelo) {
        return Ok(());
    }
    let mut tx = base::escribir(pool).await?;
    if let Some(anterior) = &anterior {
        let notas = sqlx::query("update notas set huella = null where huella is not null")
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let fragmentos = sqlx::query("update fragmentos set huella = null where huella is not null")
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tracing::info!(%anterior, nuevo = %modelo, notas, fragmentos, "cambió el modelo de huellas: se calculan de nuevo");
    }
    sqlx::query(
        "insert into ajustes (clave, valor) values ('modelo_huellas', ?1)
         on conflict (clave) do update set valor = excluded.valor",
    )
    .bind(modelo)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Una huella como está en la base: de quién es, su grupo, su fecha y sus bytes.
type HuellaGuardada = (i64, Option<i64>, DateTime<Utc>, Vec<u8>);

fn segundos(fecha: DateTime<Utc>) -> i64 {
    fecha.timestamp()
}

/// Carga en memoria las huellas guardadas en la base. Devuelve cuántas de
/// notas y cuántas de pedazos de conversación.
pub async fn cargar(pool: &SqlitePool, indice: &Indice) -> Resultado<(usize, usize)> {
    indice.vaciar();
    let notas: Vec<HuellaGuardada> = sqlx::query_as(
        "select id, grupo_id, creada, huella from notas where huella is not null and estado <> 'descartada'",
    )
    .fetch_all(pool)
    .await?;
    for (id, grupo, creada, bytes) in &notas {
        if let Some(huella) = indice::de_bytes(bytes) {
            indice.poner(Clase::Notas, Dato { id: *id, grupo: *grupo, fecha: segundos(*creada) }, &huella);
        }
    }
    let fragmentos: Vec<HuellaGuardada> =
        sqlx::query_as("select id, grupo_id, fecha, huella from fragmentos where huella is not null")
            .fetch_all(pool)
            .await?;
    for (id, grupo, fecha, bytes) in &fragmentos {
        if let Some(huella) = indice::de_bytes(bytes) {
            indice.poner(Clase::Fragmentos, Dato { id: *id, grupo: *grupo, fecha: segundos(*fecha) }, &huella);
        }
    }
    Ok((indice.cuantas(Clase::Notas), indice.cuantas(Clase::Fragmentos)))
}

/// Guarda la huella de una nota en la base y la deja lista para buscar.
pub async fn anotar_nota(e: &Estado, id: i64, grupo: i64, creada: DateTime<Utc>, huella: &[f32]) -> Resultado<()> {
    sqlx::query("update notas set huella = ?2 where id = ?1")
        .bind(id)
        .bind(indice::a_bytes(huella))
        .execute(&e.pool)
        .await?;
    e.indice.poner(Clase::Notas, Dato { id, grupo: Some(grupo), fecha: segundos(creada) }, huella);
    Ok(())
}

/// Calcula un lote de las huellas que faltan (primero notas, después pedazos
/// de conversaciones). Devuelve cuántas completó, o `None` si las huellas no
/// están andando.
pub async fn completar(e: &Estado) -> Resultado<Option<usize>> {
    let notas: Vec<(i64, i64, DateTime<Utc>, String, String)> = sqlx::query_as(
        "select id, grupo_id, creada, titulo, cuerpo from notas
         where huella is null and estado <> 'descartada' order by id limit ?1",
    )
    .bind(LOTE)
    .fetch_all(&e.pool)
    .await?;
    if !notas.is_empty() {
        let textos: Vec<String> = notas.iter().map(|(_, _, _, titulo, cuerpo)| texto_de_nota(titulo, cuerpo)).collect();
        let Some(huellas) = e.huellas_de(&textos, Uso::Texto, LIMITE_LOTE).await else {
            return Ok(None);
        };
        for ((id, grupo, creada, _, _), huella) in notas.iter().zip(&huellas) {
            anotar_nota(e, *id, *grupo, *creada, huella).await?;
        }
        return Ok(Some(notas.len()));
    }

    let fragmentos: Vec<(i64, Option<i64>, DateTime<Utc>, String)> = sqlx::query_as(
        "select id, grupo_id, fecha, contenido from fragmentos where huella is null order by id limit ?1",
    )
    .bind(LOTE)
    .fetch_all(&e.pool)
    .await?;
    if fragmentos.is_empty() {
        return Ok(Some(0));
    }
    let textos: Vec<String> = fragmentos.iter().map(|(_, _, _, contenido)| contenido.clone()).collect();
    let Some(huellas) = e.huellas_de(&textos, Uso::Texto, LIMITE_LOTE).await else {
        return Ok(None);
    };
    for ((id, grupo, fecha, _), huella) in fragmentos.iter().zip(&huellas) {
        sqlx::query("update fragmentos set huella = ?2 where id = ?1")
            .bind(id)
            .bind(indice::a_bytes(huella))
            .execute(&e.pool)
            .await?;
        e.indice.poner(Clase::Fragmentos, Dato { id: *id, grupo: *grupo, fecha: segundos(*fecha) }, huella);
    }
    Ok(Some(fragmentos.len()))
}

/// La tarea de fondo que completa las huellas que faltan: las de notas
/// guardadas mientras Ollama no respondía y las de los pedazos de
/// conversaciones que va dejando el lector.
#[must_use]
pub fn tarea(e: Estado, apagado: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        // La primera huella deja el modelo cargado (y bajado, si faltaba)
        // antes de que llegue la primera búsqueda.
        let inicio = tokio::time::Instant::now();
        tokio::select! {
            () = apagado.cancelled() => return,
            lista = e.huella_de("hola", Uso::Consulta, LIMITE_PRIMERA) => {
                if lista.is_some() {
                    let ms = u64::try_from(inicio.elapsed().as_millis()).unwrap_or(u64::MAX);
                    tracing::info!(modelo = e.huellas.modelo(), ms, "huellas listas");
                }
            }
        }
        loop {
            e.huellas.descansar();
            let espera = match completar(&e).await {
                Ok(Some(hechas)) if hechas > 0 => Duration::ZERO,
                Ok(Some(_)) => ESPERA_SIN_TRABAJO,
                Ok(None) => ESPERA_CON_FALLA,
                Err(error) => {
                    tracing::warn!(%error, "falla al completar las huellas pendientes");
                    ESPERA_CON_FALLA
                }
            };
            if espera.is_zero() {
                if apagado.is_cancelled() {
                    break;
                }
                tokio::task::yield_now().await;
                continue;
            }
            tokio::select! {
                () = apagado.cancelled() => break,
                () = e.pendientes.notified() => {},
                () = tokio::time::sleep(espera) => {},
            }
        }
    })
}
