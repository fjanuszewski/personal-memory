//! Copia la base de WhatsApp Desktop a una carpeta de la memoria, y nada más
//! (punto 4.11 del diseño).
//!
//! macOS protege los datos de las otras aplicaciones. El permiso de un
//! cartel dura lo que vive el programa que lo pidió, así que el servicio, que
//! se reinicia con cada instalación, lo tenía que pedir de nuevo. El de
//! «Acceso total al disco» queda para siempre, pero va atado al programa
//! exacto: por eso la copia la hace este, que casi nunca cambia, y no el
//! servicio. Lo arranca launchd como agente propio (si lo lanzara el
//! servicio, macOS le pediría el permiso al servicio).
//!
//! Uso: `memoria-whatsapp <base> <carpeta>`. Deja en `<carpeta>/lista` la
//! base con sus dos archivos de cambios recientes, o en `<carpeta>/error` por
//! qué no pudo. Las dos cosas aparecen de una vez (nunca a medio escribir).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Cómo se llama la base adentro de `lista`, igual que en la aplicación.
const ARCHIVO: &str = "ChatStorage.sqlite";

fn con_sufijo(archivo: &Path, sufijo: &str) -> PathBuf {
    let mut nombre = archivo.as_os_str().to_owned();
    nombre.push(sufijo);
    PathBuf::from(nombre)
}

/// Una carpeta que solo puede abrir el usuario: adentro van a estar todos los chats.
fn carpeta_propia(carpeta: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(carpeta)?;
    std::fs::set_permissions(carpeta, std::fs::Permissions::from_mode(0o700))
}

fn copiar(base: &Path, carpeta: &Path) -> Result<(), String> {
    carpeta_propia(carpeta).map_err(|e| format!("no pude crear {}: {e}", carpeta.display()))?;
    let armando = carpeta.join(format!("armando-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&armando);
    carpeta_propia(&armando).map_err(|e| format!("no pude crear {}: {e}", armando.display()))?;
    let resultado = (|| {
        std::fs::copy(base, armando.join(ARCHIVO))
            .map_err(|e| format!("no pude leer la base de WhatsApp ({}): {e}", base.display()))?;
        // Lo último que escribió la aplicación está en estos dos; si faltan, se lee lo que haya.
        for sufijo in ["-wal", "-shm"] {
            let _ = std::fs::copy(con_sufijo(base, sufijo), con_sufijo(&armando.join(ARCHIVO), sufijo));
        }
        let lista = carpeta.join("lista");
        let _ = std::fs::remove_dir_all(&lista);
        std::fs::rename(&armando, &lista).map_err(|e| format!("no pude dejar la copia en {}: {e}", lista.display()))
    })();
    if resultado.is_err() {
        let _ = std::fs::remove_dir_all(&armando);
    }
    resultado
}

/// Deja por qué no se pudo, de una vez.
fn anotar_error(carpeta: &Path, motivo: &str) {
    let nuevo = carpeta.join(format!("error-{}", std::process::id()));
    if std::fs::write(&nuevo, motivo).is_ok() {
        let _ = std::fs::rename(&nuevo, carpeta.join("error"));
    }
}

fn correr(argumentos: &[String]) -> ExitCode {
    match argumentos {
        [opcion] if opcion == "--version" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [base, carpeta] => {
            let carpeta = Path::new(carpeta);
            let _ = std::fs::remove_file(carpeta.join("error"));
            match copiar(Path::new(base), carpeta) {
                Ok(()) => ExitCode::SUCCESS,
                Err(motivo) => {
                    eprintln!("{motivo}");
                    anotar_error(carpeta, &motivo);
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("uso: memoria-whatsapp <base de WhatsApp> <carpeta donde dejar la copia>");
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    let argumentos: Vec<String> = std::env::args().skip(1).collect();
    correr(&argumentos)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn carpeta_de_prueba(nombre: &str) -> PathBuf {
        let carpeta = std::env::temp_dir().join(format!("copiador-prueba-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&carpeta);
        std::fs::create_dir_all(&carpeta).unwrap();
        carpeta
    }

    #[test]
    fn copia_la_base_con_sus_cambios_recientes() {
        let prueba = carpeta_de_prueba("copia");
        let base = prueba.join("origen").join(ARCHIVO);
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, "base").unwrap();
        std::fs::write(con_sufijo(&base, "-wal"), "cambios").unwrap();
        let carpeta = prueba.join("whatsapp");
        // Lo que había de antes se reemplaza entero.
        std::fs::create_dir_all(carpeta.join("lista")).unwrap();
        std::fs::write(carpeta.join("lista").join("vieja"), "x").unwrap();
        std::fs::write(carpeta.join("error"), "de antes").unwrap();

        let salida = correr(&[base.display().to_string(), carpeta.display().to_string()]);
        assert_eq!(salida, ExitCode::SUCCESS);
        let lista = carpeta.join("lista");
        assert_eq!(std::fs::read_to_string(lista.join(ARCHIVO)).unwrap(), "base");
        assert_eq!(std::fs::read_to_string(lista.join("ChatStorage.sqlite-wal")).unwrap(), "cambios");
        assert!(!lista.join("ChatStorage.sqlite-shm").exists());
        assert!(!lista.join("vieja").exists());
        assert!(!carpeta.join("error").exists());
        let modo = std::fs::metadata(&carpeta).unwrap().permissions().mode() & 0o777;
        assert_eq!(modo, 0o700);
        // No queda nada a medio armar.
        let sueltos: Vec<_> = std::fs::read_dir(&carpeta).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(sueltos, vec![std::ffi::OsString::from("lista")]);
        std::fs::remove_dir_all(prueba).unwrap();
    }

    #[test]
    fn si_no_puede_leer_deja_el_motivo_y_ninguna_copia() {
        let prueba = carpeta_de_prueba("falla");
        let carpeta = prueba.join("whatsapp");
        let salida = correr(&[prueba.join("no-existe.sqlite").display().to_string(), carpeta.display().to_string()]);
        assert_eq!(salida, ExitCode::FAILURE);
        let motivo = std::fs::read_to_string(carpeta.join("error")).unwrap();
        assert!(motivo.starts_with("no pude leer la base de WhatsApp"), "{motivo}");
        let sueltos: Vec<_> = std::fs::read_dir(&carpeta).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(sueltos, vec![std::ffi::OsString::from("error")]);
        std::fs::remove_dir_all(prueba).unwrap();
    }

    #[test]
    fn sin_los_dos_argumentos_explica_el_uso() {
        assert_eq!(correr(&[]), ExitCode::from(2));
        assert_eq!(correr(&["uno".to_owned()]), ExitCode::from(2));
    }
}
