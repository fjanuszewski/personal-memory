//! Respaldos: una copia entera de la base por día (se guardan 14 días) y una
//! antes de cada migración, en `~/.memoria/respaldos/`. Cada respaldo es un
//! archivo de SQLite completo: para volver a ese día alcanza con ponerlo en
//! el lugar de `memoria.db` con el servicio apagado.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use chrono::{Local, NaiveDate};
use sqlx::SqlitePool;

use crate::config::Config;

const PREFIJO: &str = "memoria-";
const EXTENSION: &str = ".db";
const CADA: Duration = Duration::from_hours(1);

#[must_use]
pub fn nombre_diario(dia: NaiveDate) -> String {
    format!("{PREFIJO}{dia}{EXTENSION}")
}

/// La fecha de un respaldo por su nombre (`memoria-2026-11-12….db`).
fn fecha_de(nombre: &str) -> Option<NaiveDate> {
    let resto = nombre.strip_prefix(PREFIJO)?;
    if !nombre.ends_with(EXTENSION) {
        return None;
    }
    NaiveDate::parse_from_str(resto.get(..10)?, "%Y-%m-%d").ok()
}

fn preparar_carpeta(carpeta: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(carpeta).with_context(|| format!("no pude crear {}", carpeta.display()))?;
    fs::set_permissions(carpeta, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Copia la base entera a `archivo`, tal como está en este momento y sin
/// frenar a quien la esté usando. Escribe aparte y renombra al terminar.
pub async fn volcar(pool: &SqlitePool, archivo: &Path) -> anyhow::Result<()> {
    if let Some(carpeta) = archivo.parent() {
        preparar_carpeta(carpeta)?;
    }
    let parcial = archivo.with_extension("parcial");
    // La copia no pisa: si quedó un intento a medias, se saca primero.
    let _ = fs::remove_file(&parcial);
    let destino = parcial.to_str().with_context(|| format!("la ruta {} no es texto", parcial.display()))?;
    if let Err(error) = sqlx::query("vacuum into ?1").bind(destino).execute(pool).await {
        let _ = fs::remove_file(&parcial);
        return Err(error).with_context(|| format!("no pude copiar la base a {}", archivo.display()));
    }
    fs::set_permissions(&parcial, fs::Permissions::from_mode(0o600))?;
    fs::rename(&parcial, archivo)?;
    Ok(())
}

/// Borra los respaldos con fecha de hace `dias` o más.
pub fn podar(carpeta: &Path, hoy: NaiveDate, dias: u32) -> std::io::Result<Vec<PathBuf>> {
    let mut borrados = Vec::new();
    for entrada in fs::read_dir(carpeta)? {
        let entrada = entrada?;
        let nombre = entrada.file_name();
        let Some(fecha) = nombre.to_str().and_then(fecha_de) else {
            continue;
        };
        if (hoy - fecha).num_days() >= i64::from(dias) {
            fs::remove_file(entrada.path())?;
            borrados.push(entrada.path());
        }
    }
    Ok(borrados)
}

/// El respaldo del día, si todavía no se hizo. Después poda los viejos.
pub async fn diario_si_falta(pool: &SqlitePool, cfg: &Config, hoy: NaiveDate) -> anyhow::Result<Option<PathBuf>> {
    let carpeta = cfg.respaldos();
    let archivo = carpeta.join(nombre_diario(hoy));
    if archivo.exists() {
        return Ok(None);
    }
    volcar(pool, &archivo).await?;
    let borrados = podar(&carpeta, hoy, cfg.dias_respaldo)?;
    if !borrados.is_empty() {
        tracing::info!(cuantos = borrados.len(), "borré respaldos viejos");
    }
    Ok(Some(archivo))
}

/// El respaldo que se hace antes de aplicar una migración.
pub async fn antes_de_migrar(pool: &SqlitePool, cfg: &Config, version: i64) -> anyhow::Result<PathBuf> {
    let ahora = Local::now();
    let archivo = cfg.respaldos().join(format!(
        "{PREFIJO}{}-antes-de-migrar-{version:04}-{}{EXTENSION}",
        ahora.date_naive(),
        ahora.format("%H%M%S"),
    ));
    volcar(pool, &archivo).await?;
    Ok(archivo)
}

/// Revisa cada hora si falta el respaldo del día.
#[must_use]
pub fn tarea_diaria(pool: SqlitePool, cfg: Config) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match diario_si_falta(&pool, &cfg, Local::now().date_naive()).await {
                Ok(Some(archivo)) => tracing::info!(archivo = %archivo.display(), "respaldo diario listo"),
                Ok(None) => {}
                Err(e) => tracing::error!(error = format!("{e:#}"), "no pude hacer el respaldo diario"),
            }
            tokio::time::sleep(CADA).await;
        }
    })
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn lee_la_fecha_del_nombre() {
        let dia = NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        assert_eq!(fecha_de(&nombre_diario(dia)), Some(dia));
        assert_eq!(fecha_de("memoria-2026-11-12-antes-de-migrar-0002-101500.db"), Some(dia));
        assert_eq!(fecha_de("otra-cosa.db"), None);
        assert_eq!(fecha_de("memoria-2026-11-12.parcial"), None);
    }

    #[test]
    fn guarda_catorce_dias() {
        let carpeta = std::env::temp_dir().join(format!("memoria-respaldos-{}", std::process::id()));
        let _ = fs::remove_dir_all(&carpeta);
        fs::create_dir_all(&carpeta).unwrap();
        let hoy = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        for dias in 0..20 {
            let dia = hoy - chrono::Days::new(dias);
            fs::write(carpeta.join(nombre_diario(dia)), b"x").unwrap();
        }
        fs::write(carpeta.join("notas.txt"), b"x").unwrap();

        let borrados = podar(&carpeta, hoy, 14).unwrap();
        assert_eq!(borrados.len(), 6);
        let quedan = fs::read_dir(&carpeta).unwrap().count();
        assert_eq!(quedan, 15); // 14 respaldos y el archivo ajeno
        assert!(carpeta.join(nombre_diario(hoy - chrono::Days::new(13))).exists());
        assert!(!carpeta.join(nombre_diario(hoy - chrono::Days::new(14))).exists());
        fs::remove_dir_all(&carpeta).unwrap();
    }
}
