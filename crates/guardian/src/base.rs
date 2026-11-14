//! Copias y respaldos de la base. La base es un archivo de SQLite: una copia
//! es otro archivo, y volver atrás es poner el respaldo en su lugar.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{ConnectOptions, Connection, SqliteConnection, SqlitePool};

use crate::config::Config;

/// Cuánto se espera el turno si otro programa está escribiendo.
const ESPERA: Duration = Duration::from_secs(30);

fn opciones(archivo: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new().filename(archivo).foreign_keys(true).busy_timeout(ESPERA)
}

/// Abre la base real, la misma que usa el servicio. No la crea: eso lo hace
/// el servicio al arrancar.
pub async fn abrir(archivo: &Path) -> anyhow::Result<SqlitePool> {
    SqlitePoolOptions::new()
        .max_connections(4)
        .min_connections(0)
        .idle_timeout(Duration::from_secs(30))
        .connect_with(opciones(archivo).journal_mode(SqliteJournalMode::Wal))
        .await
        .with_context(|| format!("no pude abrir la base {}", archivo.display()))
}

/// Una conexión suelta a una base (la real o una copia), para una consulta.
pub async fn conexion(archivo: &Path) -> anyhow::Result<SqliteConnection> {
    opciones(archivo).connect().await.with_context(|| format!("no pude abrir la base {}", archivo.display()))
}

/// El archivo con `sufijo` pegado al nombre (`memoria.db` + `-wal`).
fn con_sufijo(archivo: &Path, sufijo: &str) -> PathBuf {
    let mut nombre = OsString::from(archivo.as_os_str());
    nombre.push(sufijo);
    PathBuf::from(nombre)
}

/// Los archivos que SQLite deja al lado de una base mientras se usa. Van
/// siempre con ella: una base con los acompañantes de otra se rompe.
fn acompanantes(archivo: &Path) -> [PathBuf; 2] {
    [con_sufijo(archivo, "-wal"), con_sufijo(archivo, "-shm")]
}

/// Borra una copia de la base, con sus acompañantes. La real no se borra nunca.
pub fn borrar(cfg: &Config, archivo: &Path) -> anyhow::Result<()> {
    if archivo == cfg.bd {
        bail!("la base real no se borra");
    }
    for parte in [archivo.to_owned()].into_iter().chain(acompanantes(archivo)) {
        match std::fs::remove_file(&parte) {
            Ok(()) => {}
            Err(falla) if falla.kind() == std::io::ErrorKind::NotFound => {}
            Err(falla) => return Err(falla).with_context(|| format!("no pude borrar {}", parte.display())),
        }
    }
    Ok(())
}

/// Guarda en `destino` una copia entera de la base `origen`, tal como está en
/// este momento, aunque el servicio la esté usando.
pub async fn volcar(cfg: &Config, origen: &Path, destino: &Path) -> anyhow::Result<()> {
    if let Some(carpeta) = destino.parent() {
        std::fs::create_dir_all(carpeta).with_context(|| format!("no pude crear {}", carpeta.display()))?;
    }
    // La copia no pisa un archivo que ya esté.
    borrar(cfg, destino)?;
    let texto = destino.to_str().with_context(|| format!("la ruta {} no es texto", destino.display()))?;
    let mut origen_abierto = conexion(origen).await?;
    let copia = sqlx::query("vacuum into ?1").bind(texto).execute(&mut origen_abierto).await;
    let _ = origen_abierto.close().await;
    copia.with_context(|| format!("no pude copiar la base a {}", destino.display()))?;
    std::fs::set_permissions(destino, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Deja en `destino` una copia de la base guardada en `archivo`.
pub async fn copiar_desde(cfg: &Config, archivo: &Path, destino: &Path) -> anyhow::Result<()> {
    borrar(cfg, destino)?;
    tokio::fs::copy(archivo, destino)
        .await
        .with_context(|| format!("no pude copiar {} a {}", archivo.display(), destino.display()))?;
    Ok(())
}

/// La última migración aplicada en una base (0 si no tiene ninguna).
pub async fn version_migraciones(bd: &Path) -> anyhow::Result<i64> {
    let mut abierta = conexion(bd).await.context("no pude abrir la base para ver las migraciones")?;
    let version: Result<Option<i64>, sqlx::Error> =
        sqlx::query_scalar("select max(version) from _sqlx_migrations where success").fetch_one(&mut abierta).await;
    let _ = abierta.close().await;
    Ok(version?.unwrap_or(0))
}

/// Mueve una base con sus acompañantes.
fn mover(desde: &Path, hasta: &Path) -> anyhow::Result<()> {
    std::fs::rename(desde, hasta)
        .with_context(|| format!("no pude mover {} a {}", desde.display(), hasta.display()))?;
    for (parte, destino) in acompanantes(desde).into_iter().zip(acompanantes(hasta)) {
        match std::fs::rename(&parte, &destino) {
            Ok(()) => {}
            Err(falla) if falla.kind() == std::io::ErrorKind::NotFound => {}
            Err(falla) => return Err(falla).with_context(|| format!("no pude mover {}", parte.display())),
        }
    }
    Ok(())
}

/// Vuelve la base real a lo guardado en `respaldo`. Nadie puede estar
/// usándola (el servicio tiene que estar apagado). La base que falló no se
/// borra: queda al lado, con otro nombre, por si hay que mirarla. Devuelve
/// dónde quedó.
pub async fn volver_a(cfg: &Config, respaldo: &Path, apodo: &str) -> anyhow::Result<PathBuf> {
    let real = &cfg.bd;
    if !respaldo.is_file() {
        bail!("no encuentro el respaldo {}", respaldo.display());
    }
    let fallida = con_sufijo(real, &format!(".fallida-{apodo}"));
    mover(real, &fallida)?;
    if let Err(falla) = tokio::fs::copy(respaldo, real).await {
        // No se pudo restaurar: que vuelva la que estaba, tal como quedó.
        let _ = std::fs::remove_file(real);
        mover(&fallida, real)?;
        return Err(anyhow::Error::new(falla).context("no pude restaurar la base del respaldo; quedó la que estaba"));
    }
    Ok(fallida)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    async fn valores(archivo: &Path) -> Vec<String> {
        let mut abierta = conexion(archivo).await.unwrap();
        let valores = sqlx::query_scalar("select valor from cosas order by id").fetch_all(&mut abierta).await.unwrap();
        abierta.close().await.unwrap();
        valores
    }

    #[tokio::test]
    async fn copia_la_base_en_uso_y_vuelve_al_respaldo() {
        let dir = std::env::temp_dir().join(format!("memoria-guardian-base-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = Config::de_prueba(&dir);

        // El servicio, con la base abierta y cosas escritas que todavía no
        // pasaron al archivo principal.
        let servicio = SqlitePoolOptions::new()
            .connect_with(opciones(&cfg.bd).create_if_missing(true).journal_mode(SqliteJournalMode::Wal))
            .await
            .unwrap();
        sqlx::query("create table cosas (id integer primary key, valor text not null)")
            .execute(&servicio)
            .await
            .unwrap();
        sqlx::query("create table _sqlx_migrations (version integer primary key, success integer not null)")
            .execute(&servicio)
            .await
            .unwrap();
        sqlx::query("insert into _sqlx_migrations values (1, 1), (2, 1), (3, 0)").execute(&servicio).await.unwrap();
        sqlx::query("insert into cosas (valor) values ('antes')").execute(&servicio).await.unwrap();
        assert_eq!(version_migraciones(&cfg.bd).await.unwrap(), 2);

        // El respaldo se hace con el servicio prendido y trae todo.
        let respaldo = dir.join("respaldos/antes.db");
        volcar(&cfg, &cfg.bd, &respaldo).await.unwrap();
        assert_eq!(valores(&respaldo).await, ["antes"]);
        // Volver a copiar al mismo lugar no falla: pisa la copia anterior.
        volcar(&cfg, &cfg.bd, &respaldo).await.unwrap();
        let copia = dir.join("trabajo/copia.db");
        std::fs::create_dir_all(copia.parent().unwrap()).unwrap();
        copiar_desde(&cfg, &respaldo, &copia).await.unwrap();
        assert_eq!(valores(&copia).await, ["antes"]);

        // Una instalación que sale mal: la base cambia y el servicio se apaga.
        sqlx::query("insert into cosas (valor) values ('con la mejora fallida')").execute(&servicio).await.unwrap();
        servicio.close().await;
        let fallida = volver_a(&cfg, &respaldo, "mejora_7").await.unwrap();
        assert_eq!(valores(&cfg.bd).await, ["antes"], "la base real volvió al respaldo");
        assert_eq!(valores(&fallida).await, ["antes", "con la mejora fallida"], "la fallida queda para mirarla");
        assert_eq!(fallida, dir.join("memoria.db.fallida-mejora_7"));

        // La real no se borra; las copias sí, con sus acompañantes.
        assert!(borrar(&cfg, &cfg.bd).is_err());
        assert!(cfg.bd.is_file());
        borrar(&cfg, &copia).unwrap();
        borrar(&cfg, &copia).unwrap();
        assert!(!copia.exists() && acompanantes(&copia).iter().all(|a| !a.exists()));
        // Sin respaldo no se toca nada.
        assert!(volver_a(&cfg, &dir.join("no-existe.db"), "x").await.is_err());
        assert_eq!(valores(&cfg.bd).await, ["antes"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn los_acompanantes_van_pegados_al_nombre() {
        let base = Path::new("/tmp/memoria.db");
        assert_eq!(con_sufijo(base, ".fallida-mejora_3"), Path::new("/tmp/memoria.db.fallida-mejora_3"));
        assert_eq!(acompanantes(base), [PathBuf::from("/tmp/memoria.db-wal"), PathBuf::from("/tmp/memoria.db-shm")]);
    }
}
