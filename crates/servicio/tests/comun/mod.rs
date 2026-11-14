//! Lo que comparten las pruebas: una base nueva para cada una, unas huellas
//! de mentira (para no depender de un modelo) y cómo empezar una conversación
//! y armar pedidos.
#![allow(dead_code, reason = "cada archivo de pruebas usa una parte")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use memoria::estado::Estado;
use memoria::grupos::Registro;
use memoria::huellas::{DIMENSIONES, Falla, Futuro, Huellas, Uso};
use memoria::mcp::{self, PedidoBuscar, PedidoContexto, PedidoGuardar};
use memoria::modelo::TipoNota;
use sqlx::SqlitePool;

const VACIAS: &[&str] = &[
    "el", "la", "los", "las", "de", "del", "que", "y", "a", "en", "un", "una", "por", "para", "con", "se", "es", "al",
    "lo", "su", "o", "e",
];

/// Palabras que para estas huellas quieren decir lo mismo. Pesan más que el
/// resto, como en un modelo de verdad pesa de qué trata el texto.
const CONCEPTOS: &[(&str, &[&str])] = &[
    ("acceso", &["login", "sesion", "autenticacion", "ingreso"]),
    ("correo", &["mail", "mails", "correo", "correos", "sendgrid"]),
];

fn palabras(texto: &str) -> Vec<(String, f32)> {
    let limpio: String = texto
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    let mut salida: Vec<(String, f32)> = Vec::new();
    for palabra in limpio.split_whitespace().filter(|p| !VACIAS.contains(p)) {
        let (clave, peso) = match CONCEPTOS.iter().find(|(_, iguales)| iguales.contains(&palabra)) {
            Some((concepto, _)) => ((*concepto).to_owned(), 3.0),
            // «entregas» y «entrega» cuentan como la misma palabra.
            None if palabra.len() > 4 && palabra.ends_with('s') => (palabra[..palabra.len() - 1].to_owned(), 1.0),
            None => (palabra.to_owned(), 1.0),
        };
        if !salida.iter().any(|(c, _)| *c == clave) {
            salida.push((clave, peso));
        }
    }
    salida
}

/// Una huella armada con las palabras del texto: dos textos quedan cerca si
/// comparten palabras o conceptos, y lejos si no comparten nada.
pub fn huella(texto: &str) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIMENSIONES];
    for (palabra, peso) in palabras(texto) {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in palabra.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        v[usize::try_from(h % 1024).unwrap()] += peso;
    }
    let norma = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norma > 0.0 {
        for x in &mut v {
            *x /= norma;
        }
    } else {
        v[0] = 1.0;
    }
    v
}

pub struct DePalabras;

impl Huellas for DePalabras {
    fn modelo(&self) -> &'static str {
        "de-palabras"
    }

    fn calcular<'a>(&'a self, textos: &'a [String], _: Uso) -> Futuro<'a, Result<Vec<Vec<f32>>, Falla>> {
        Box::pin(async move { Ok(textos.iter().map(|t| huella(t)).collect()) })
    }
}

/// Como si el modelo de huellas no respondiera.
pub struct Caidas;

impl Huellas for Caidas {
    fn modelo(&self) -> &'static str {
        "caidas"
    }

    fn calcular<'a>(&'a self, _: &'a [String], _: Uso) -> Futuro<'a, Result<Vec<Vec<f32>>, Falla>> {
        Box::pin(async { Err(Falla::Conexion("apagado".into())) })
    }
}

/// Una base recién creada, en una carpeta propia que se borra al terminar.
pub struct Base {
    pub pool: SqlitePool,
    pub carpeta: PathBuf,
}

impl Base {
    pub async fn nueva() -> Self {
        static CUENTA: AtomicU32 = AtomicU32::new(0);
        let numero = CUENTA.fetch_add(1, Ordering::Relaxed);
        let carpeta = std::env::temp_dir().join(format!("memoria-prueba-{}-{numero}", std::process::id()));
        let _ = std::fs::remove_dir_all(&carpeta);
        let pool = memoria::base::abrir(&carpeta.join("memoria.db")).await.unwrap();
        memoria::MIGRACIONES.run(&pool).await.unwrap();
        memoria::textos_al_dia(&pool).await.unwrap();
        Self { pool, carpeta }
    }

    /// El archivo de la base.
    pub fn archivo(&self) -> PathBuf {
        self.carpeta.join("memoria.db")
    }
}

impl Drop for Base {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.carpeta);
    }
}

/// El servicio recién arrancado sobre `pool`: con las huellas que ya había
/// guardadas cargadas en memoria.
async fn arrancado(pool: SqlitePool, huellas: Arc<dyn Huellas>) -> Estado {
    let e = Estado::nuevo(pool, huellas);
    memoria::huellas::cargar(&e.pool, &e.indice).await.unwrap();
    e
}

pub async fn estado(pool: SqlitePool) -> Estado {
    arrancado(pool, Arc::new(DePalabras)).await
}

pub async fn estado_sin_huellas(pool: SqlitePool) -> Estado {
    arrancado(pool, Arc::new(Caidas)).await
}

pub fn registro(empresa: &str, proyecto: Option<&str>, grupo_app: &str) -> Registro {
    Registro {
        empresa: Some(empresa.into()),
        proyecto: proyecto.map(Into::into),
        grupo_app: Some(grupo_app.into()),
        carpeta: Some("/Users/ana/GITHUB/acme/portal".into()),
        titulo: Some("Conversación de prueba".into()),
        transcripcion: None,
    }
}

/// Lo que hace el mod al empezar una conversación: la registra y recibe el resumen.
pub async fn empezar(e: &Estado, sesion: &str, empresa: &str, proyecto: Option<&str>) -> String {
    let grupo_app = match proyecto {
        Some(p) => format!("[{empresa}] - {}", p.to_uppercase()),
        None => format!("[{}]", empresa.to_uppercase()),
    };
    mcp::contexto(
        e,
        &PedidoContexto { sesion: sesion.into(), grupo: None, registro: Some(registro(empresa, proyecto, &grupo_app)) },
    )
    .await
    .unwrap()
}

pub fn nota(tipo: TipoNota, titulo: &str, cuerpo: &str, sesion: &str) -> PedidoGuardar {
    PedidoGuardar {
        tipo,
        titulo: titulo.into(),
        cuerpo: cuerpo.into(),
        sesion: Some(sesion.into()),
        grupo: None,
        repos: vec!["portal-web".into()],
        etiquetas: vec!["Auth".into()],
        reemplaza: None,
    }
}

pub fn busqueda(consulta: &str, sesion: &str) -> PedidoBuscar {
    PedidoBuscar {
        consulta: consulta.into(),
        sesion: Some(sesion.into()),
        grupo: None,
        tipo: None,
        desde: None,
        conversaciones: true,
        reemplazadas: false,
        limite: None,
    }
}
