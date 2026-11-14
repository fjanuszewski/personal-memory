//! El encargo para el agente de mantenimiento.
//!
//! Al agente no lo lanza el guardián: corre de fondo dentro de una
//! conversación de Claude, lanzado por el mod, con la sesión de Claude que ya
//! está abierta (sin ninguna llave). El guardián arma la copia del código,
//! deja acá lo que el mod necesita (la carpeta, el `cargo` de la mejora y el
//! pedido con las reglas) y espera a que el agente termine para revisar,
//! probar, medir e instalar.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;

use crate::config::Config;
use crate::git;
use crate::mejora::Mejora;

/// Las reglas fijas del agente. Están en el guardián para que el agente no
/// las pueda cambiar; viajan al principio de cada pedido.
pub const INSTRUCCIONES: &str = include_str!("../instrucciones.md");

/// Lo que recibe el mod de la conversación que lanza al agente. Se guarda
/// como texto JSON en la columna `encargo`; el servicio lo pasa tal cual.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Encargo {
    /// La carpeta de la copia del código: ahí trabaja el agente, y de ahí no sale.
    pub copia: String,
    /// El único programa que el agente puede correr: el `cargo` de esta mejora.
    pub cargo: String,
    /// Las reglas y el reporte de la falla.
    pub pedido: String,
}

fn si_o_no(valor: bool) -> &'static str {
    if valor { "sí" } else { "no" }
}

/// El reporte de la falla y lo que el arreglo tiene permitido.
#[must_use]
pub fn reporte(m: &Mejora) -> String {
    let mut texto = format!("Mejora {} de la memoria.\n\nQué falla: {}\n\nDetalle: {}\n", m.id, m.resumen, m.detalle);
    let dato = |clave: &str| m.ejemplo[clave].as_str().map(str::trim).filter(|t| !t.is_empty());
    if let Some(buscado) = dato("buscado") {
        let _ = writeln!(texto, "\nQué se buscó: «{buscado}»");
    }
    if let Some(grupo) = &m.grupo {
        let _ = writeln!(texto, "En el grupo: {grupo}");
    }
    if let Some(esperado) = dato("esperado") {
        let _ = writeln!(texto, "Qué se esperaba: {esperado}");
    }
    let notas: Vec<String> =
        m.ejemplo["notas_esperadas"].as_array().into_iter().flatten().map(ToString::to_string).collect();
    if !notas.is_empty() {
        let _ = writeln!(texto, "Notas que tenían que aparecer (ids en la base): {}", notas.join(", "));
    }
    if let Some(obtenido) = dato("obtenido") {
        let _ = writeln!(texto, "Qué apareció: {obtenido}");
    }
    let _ = write!(
        texto,
        "\nPermisos de este arreglo: cambiar la estructura de la base: {}; sumar librerías: {}; borrar datos: {}.",
        si_o_no(m.permisos.toca_base),
        si_o_no(m.permisos.suma_librerias),
        si_o_no(m.permisos.borra_datos)
    );
    texto
}

/// Lo que se le pide al agente: las reglas fijas y, después, el reporte.
#[must_use]
pub fn pedido(m: &Mejora) -> String {
    format!("{}\n\n## El pedido\n\n{}", INSTRUCCIONES.trim_end(), reporte(m))
}

/// Donde queda el `cargo` de una mejora: al lado de su copia del código y
/// fuera de ella, para que el agente no lo pueda editar.
#[must_use]
pub fn cargo_de(copia: &Path) -> PathBuf {
    let nombre = copia.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    copia.with_file_name(format!("{nombre}-cargo"))
}

/// Un texto entre comillas simples para `sh`.
fn entre_comillas(texto: &str) -> String {
    format!("'{}'", texto.replace('\'', r"'\''"))
}

/// El programita que hace de `cargo` para el agente: lo corre en la copia,
/// con el mismo entorno mínimo que usa el guardián (nada de la conversación
/// de Claude pasa a lo que se compila o se prueba) y compilando en la carpeta
/// de la mejora. Sin permiso para sumar librerías, tampoco tiene red.
#[must_use]
pub fn guion_de_cargo(cfg: &Config, copia: &Path, con_red: bool) -> String {
    let mut entorno = cfg.entorno();
    entorno.push(("CARGO_TARGET_DIR", git::compilados_de(copia).display().to_string()));
    if !con_red {
        entorno.push(("CARGO_NET_OFFLINE", "true".to_owned()));
    }
    let variables: Vec<String> =
        entorno.iter().map(|(nombre, valor)| format!("{nombre}={}", entre_comillas(valor))).collect();
    format!(
        "#!/bin/sh\n\
         # Lo deja el guardián de la memoria: el `cargo` del agente de esta mejora.\n\
         cd {copia} || exit 1\n\
         exec /usr/bin/env -i {variables} ${{RUST_BACKTRACE:+\"RUST_BACKTRACE=$RUST_BACKTRACE\"}} cargo \"$@\"\n",
        copia = entre_comillas(&copia.display().to_string()),
        variables = variables.join(" "),
    )
}

/// Deja el `cargo` de la mejora listo para usar. Devuelve dónde quedó.
pub fn dejar_cargo(cfg: &Config, copia: &Path, con_red: bool) -> anyhow::Result<PathBuf> {
    let archivo = cargo_de(copia);
    std::fs::write(&archivo, guion_de_cargo(cfg, copia, con_red))
        .with_context(|| format!("no pude escribir {}", archivo.display()))?;
    std::fs::set_permissions(&archivo, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("no pude dejar {} como programa", archivo.display()))?;
    Ok(archivo)
}

impl Encargo {
    /// Arma el encargo de una mejora cuya copia del código ya está en `copia`.
    pub fn armar(cfg: &Config, m: &Mejora, copia: &Path) -> anyhow::Result<Self> {
        let cargo = dejar_cargo(cfg, copia, m.permisos.suma_librerias)?;
        Ok(Self { copia: copia.display().to_string(), cargo: cargo.display().to_string(), pedido: pedido(m) })
    }
}

#[cfg(test)]
mod pruebas {
    use serde_json::json;

    use super::*;
    use crate::revision::Permisos;

    fn mejora() -> Mejora {
        Mejora {
            id: 7,
            resumen: "La búsqueda de «entregas» no trae el acuerdo.".into(),
            detalle: "Busqué eso y no apareció nada.".into(),
            ejemplo: json!({ "buscado": "cuándo se entrega", "esperado": "la nota del acuerdo", "obtenido": "nada", "notas_esperadas": [4, 9] }),
            grupo_id: Some(2),
            grupo: Some("ACME › Portal".into()),
            permisos: Permisos { toca_base: true, ..Permisos::default() },
            intentos: 1,
            lanzamientos: 0,
            informe: None,
            desde_version: None,
            rama: None,
        }
    }

    #[test]
    fn el_reporte_trae_el_ejemplo_y_los_permisos() {
        let mut m = mejora();
        let texto = reporte(&m);
        for parte in [
            "Mejora 7 de la memoria.",
            "Qué falla: La búsqueda de «entregas» no trae el acuerdo.",
            "Qué se buscó: «cuándo se entrega»",
            "En el grupo: ACME › Portal",
            "Notas que tenían que aparecer (ids en la base): 4, 9",
            "Qué apareció: nada",
            "cambiar la estructura de la base: sí; sumar librerías: no; borrar datos: no.",
        ] {
            assert!(texto.contains(parte), "falta «{parte}» en:\n{texto}");
        }
        // Un reporte sin ejemplo no inventa renglones.
        m.ejemplo = json!({ "buscado": null, "notas_esperadas": [] });
        m.grupo = None;
        let texto = reporte(&m);
        assert!(
            !texto.contains("Qué se buscó") && !texto.contains("En el grupo") && !texto.contains("Notas que"),
            "{texto}"
        );
    }

    #[test]
    fn el_pedido_empieza_por_las_reglas() {
        let texto = pedido(&mejora());
        assert!(texto.starts_with("Sos el agente de mantenimiento"), "{texto}");
        let (reglas, reporte) = texto.split_once("\n\n## El pedido\n\n").expect("las reglas y, después, el pedido");
        assert!(reporte.starts_with("Mejora 7 de la memoria."));
        // Las reglas nombran lo que no se toca y cómo se usa `cargo`.
        for protegido in ["crates/guardian/", "bateria/", "instalacion/", "mod/", "sin tuberías"] {
            assert!(reglas.contains(protegido), "{protegido}");
        }
    }

    #[test]
    fn el_cargo_del_agente_corre_en_la_copia_con_el_entorno_minimo() {
        let dir = std::env::temp_dir().join(format!("memoria-guardian-encargo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Una carpeta con espacio y comilla en el nombre: tiene que andar igual.
        let copia = dir.join("trabajo de 'prueba'/mejora-7");
        std::fs::create_dir_all(&copia).unwrap();
        let cfg = Config::de_prueba(&dir);
        assert_eq!(cargo_de(&copia), dir.join("trabajo de 'prueba'/mejora-7-cargo"));

        let guion = guion_de_cargo(&cfg, &copia, false);
        assert!(guion.starts_with("#!/bin/sh\n"));
        assert!(guion.contains("/usr/bin/env -i HOME="), "{guion}");
        assert!(guion.contains("CARGO_NET_OFFLINE='true'"), "{guion}");
        assert!(guion.contains("mejora-7-compilado'"), "{guion}");
        assert!(!guion_de_cargo(&cfg, &copia, true).contains("CARGO_NET_OFFLINE"));

        // De verdad: un `cargo` de mentira dice dónde corre y qué le llega.
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let falso = bin.join("cargo");
        std::fs::write(
            &falso,
            "#!/bin/sh\necho \"en=$(pwd -P)\"\necho \"args=$*\"\necho \"destino=$CARGO_TARGET_DIR\"\n\
             echo \"red=$CARGO_NET_OFFLINE\"\necho \"rastro=$RUST_BACKTRACE\"\necho \"secreto=$SECRETO\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&falso, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = cfg;
        cfg.ruta = format!("{}:/usr/bin:/bin", bin.display());
        let m = mejora();
        let encargo = Encargo::armar(&cfg, &m, &copia).unwrap();
        assert_eq!(encargo.copia, copia.display().to_string());
        let permisos = std::fs::metadata(&encargo.cargo).unwrap().permissions().mode() & 0o777;
        assert_eq!(permisos, 0o700);
        let salida = std::process::Command::new(&encargo.cargo)
            .args(["test", "-p", "memoria"])
            .current_dir("/")
            .env("SECRETO", "no tiene que pasar")
            .env("RUST_BACKTRACE", "1")
            .output()
            .unwrap();
        let dicho = String::from_utf8_lossy(&salida.stdout);
        assert!(salida.status.success(), "{dicho}{}", String::from_utf8_lossy(&salida.stderr));
        let real = copia.canonicalize().unwrap();
        assert!(dicho.contains(&format!("en={}\n", real.display())), "{dicho}");
        assert!(dicho.contains("args=test -p memoria\n"), "{dicho}");
        assert!(dicho.contains("mejora-7-compilado\n"), "{dicho}");
        assert!(dicho.contains("red=true\n") && dicho.contains("rastro=1\n"), "{dicho}");
        assert!(dicho.contains("secreto=\n"), "nada del entorno de quien lo llama pasa: {dicho}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
