//! La clave local que pide el servicio: se crea una vez en `~/.memoria/clave`
//! (solo legible por el usuario) y la usa el alta del MCP.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, bail};

const LARGO_MINIMO: usize = 32;

/// Lee la clave o, si no existe, crea una nueva al azar.
pub fn cargar_o_crear(ruta: &Path) -> anyhow::Result<String> {
    match fs::read_to_string(ruta) {
        Ok(texto) => {
            let clave = texto.trim().to_owned();
            if clave.len() < LARGO_MINIMO {
                bail!("la clave en {} es demasiado corta", ruta.display());
            }
            Ok(clave)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => crear(ruta),
        Err(e) => Err(e).with_context(|| format!("no pude leer {}", ruta.display())),
    }
}

fn crear(ruta: &Path) -> anyhow::Result<String> {
    if let Some(carpeta) = ruta.parent() {
        fs::create_dir_all(carpeta).with_context(|| format!("no pude crear {}", carpeta.display()))?;
        fs::set_permissions(carpeta, fs::Permissions::from_mode(0o700))?;
    }
    let mut azar = [0u8; 32];
    fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut azar)).context("no pude leer /dev/urandom")?;
    let clave = azar.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    let mut archivo = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(ruta)
        .with_context(|| format!("no pude crear {}", ruta.display()))?;
    archivo.write_all(clave.as_bytes())?;
    Ok(clave)
}

/// Compara sin cortar en el primer byte distinto, para no dar pistas por el
/// tiempo de respuesta.
#[must_use]
pub fn iguales(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn crea_la_clave_una_vez_y_despues_la_lee() {
        let carpeta = std::env::temp_dir().join(format!("memoria-clave-{}", std::process::id()));
        let _ = fs::remove_dir_all(&carpeta);
        let ruta = carpeta.join("clave");

        let primera = cargar_o_crear(&ruta).unwrap();
        assert_eq!(primera.len(), 64);
        assert_eq!(cargar_o_crear(&ruta).unwrap(), primera);
        let modo = fs::metadata(&ruta).unwrap().permissions().mode() & 0o777;
        assert_eq!(modo, 0o600);
        fs::remove_dir_all(&carpeta).unwrap();
    }

    #[test]
    fn compara_claves() {
        assert!(iguales(b"abc", b"abc"));
        assert!(!iguales(b"abc", b"abd"));
        assert!(!iguales(b"abc", b"abcd"));
    }
}
