//! La base: un archivo de SQLite. Acá está cómo se abre y las pocas cosas
//! que SQLite no trae hechas (listas, fechas, transacciones de escritura).

use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::error::Resultado;

/// Abre (o crea) la base en `archivo`. Varios programas pueden leerla a la
/// vez; si dos quieren escribir, el segundo espera su turno.
pub async fn abrir(archivo: &Path) -> anyhow::Result<SqlitePool> {
    if let Some(carpeta) = archivo.parent() {
        fs::create_dir_all(carpeta).with_context(|| format!("no pude crear {}", carpeta.display()))?;
        // Adentro hay conversaciones: que solo las pueda leer su dueño.
        let _ = fs::set_permissions(carpeta, fs::Permissions::from_mode(0o700));
    }
    // El archivo nace cerrado para los demás usuarios de la máquina, y los
    // que SQLite deja al lado heredan ese permiso.
    match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(archivo) {
        Ok(_) => {}
        Err(falla) if falla.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::set_permissions(archivo, fs::Permissions::from_mode(0o600));
        }
        Err(falla) => return Err(falla).with_context(|| format!("no pude crear la base {}", archivo.display())),
    }
    let opciones = SqliteConnectOptions::new()
        .filename(archivo)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(15));
    SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opciones)
        .await
        .with_context(|| format!("no pude abrir la base {}", archivo.display()))
}

/// Empieza una transacción que va a escribir. Toma el turno de escritura
/// desde el principio: así no falla a mitad de camino si otro escribió antes.
pub async fn escribir(pool: &SqlitePool) -> Resultado<Transaction<'static, Sqlite>> {
    Ok(pool.begin_with("begin immediate").await?)
}

/// La hora de ahora, como se guarda en la base.
#[must_use]
pub fn ahora() -> DateTime<Utc> {
    Utc::now()
}

/// Lee una lista guardada como texto JSON. Si está rota, vacía.
#[must_use]
pub fn lista(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Lee una lista de ids guardada como texto JSON.
#[must_use]
pub fn ids(json: &str) -> Vec<i64> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Una lista como texto JSON, para guardarla o para pasarla a una consulta
/// (`… in (select value from json_each(?))`).
#[must_use]
pub fn a_json<T: serde::Serialize>(valores: &[T]) -> String {
    serde_json::to_string(valores).unwrap_or_else(|_| "[]".to_owned())
}

/// Suma a `lista` los valores de `nuevos` que no estaban, sin repetir.
#[must_use]
pub fn unir(lista: &[String], nuevos: &[String]) -> Vec<String> {
    let mut salida = lista.to_vec();
    for nuevo in nuevos {
        if !salida.contains(nuevo) {
            salida.push(nuevo.clone());
        }
    }
    salida
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn las_listas_van_y_vienen_como_texto() {
        let lista_de_prueba = vec!["local_a".to_owned(), "con \"comillas\"".to_owned()];
        assert_eq!(lista(&a_json(&lista_de_prueba)), lista_de_prueba);
        assert_eq!(ids(&a_json(&[3_i64, 5])), vec![3, 5]);
        assert_eq!(lista("esto no es json"), Vec::<String>::new());
        assert_eq!(a_json::<String>(&[]), "[]");
    }

    #[test]
    fn une_sin_repetir_y_en_orden() {
        let unida = unir(&["a".into(), "b".into()], &["b".into(), "c".into()]);
        assert_eq!(unida, vec!["a", "b", "c"]);
    }
}
