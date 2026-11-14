//! Lo que el guardián revisa del cambio del agente antes de compilar nada:
//! que solo haya tocado lo que puede y dentro de lo que el usuario aceptó.

use std::sync::LazyLock;

use regex::Regex;

/// Lo que el usuario aceptó que el arreglo puede necesitar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Permisos {
    pub toca_base: bool,
    pub borra_datos: bool,
    pub suma_librerias: bool,
}

/// Un archivo que cambió en la copia, como lo informa `git status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cambio {
    /// `M` modificado, `A` o `??` nuevo, `D` borrado, `R` renombrado.
    pub estado: String,
    pub ruta: String,
}

impl Cambio {
    fn es_nuevo(&self) -> bool {
        self.estado == "??" || self.estado.starts_with('A')
    }
}

/// Donde el agente puede escribir. Todo lo demás está protegido: el
/// guardián, la batería, la instalación, el mod y la raíz del repositorio.
const PERMITIDAS: [&str; 2] = ["crates/servicio/", "migraciones/"];
const LIBRERIAS: [&str; 2] = ["Cargo.lock", "crates/servicio/Cargo.toml"];

static BORRA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(drop\s+(table|column|schema|database|type|extension|index)|delete\s+from|truncate)\b")
        .expect("patrón válido")
});

/// Lee la salida de `git status --porcelain=v1 -uall`.
#[must_use]
pub fn leer_cambios(estado: &str) -> Vec<Cambio> {
    let mut cambios = Vec::new();
    for renglon in estado.lines().filter(|r| r.len() > 3) {
        let (codigo, resto) = renglon.split_at(3);
        let codigo = codigo.trim().to_owned();
        // Un renombre viene como «viejo -> nuevo»: cuentan los dos.
        for ruta in resto.split(" -> ") {
            cambios.push(Cambio { estado: codigo.clone(), ruta: ruta.trim().trim_matches('"').to_owned() });
        }
    }
    cambios
}

/// Revisa qué archivos cambió el agente. `Err` trae el motivo, dicho para el usuario.
pub fn revisar_rutas(cambios: &[Cambio], permisos: Permisos) -> Result<(), String> {
    let protegidos: Vec<&str> = cambios
        .iter()
        .map(|c| c.ruta.as_str())
        .filter(|r| !PERMITIDAS.iter().any(|p| r.starts_with(p)) && !LIBRERIAS.contains(r))
        .collect();
    if !protegidos.is_empty() {
        return Err(format!("El agente tocó archivos que no puede tocar: {}.", protegidos.join(", ")));
    }
    if !permisos.suma_librerias && cambios.iter().any(|c| LIBRERIAS.contains(&c.ruta.as_str())) {
        return Err("El agente sumó o cambió librerías, y esta mejora no tenía permiso para eso.".to_owned());
    }
    let migraciones: Vec<&Cambio> = cambios.iter().filter(|c| c.ruta.starts_with("migraciones/")).collect();
    if let Some(vieja) = migraciones.iter().find(|c| !c.es_nuevo()) {
        return Err(format!("El agente cambió una migración que ya estaba aplicada ({}).", vieja.ruta));
    }
    if !migraciones.is_empty() && !permisos.toca_base {
        return Err("El agente cambió la estructura de la base, y esta mejora no tenía permiso para eso.".to_owned());
    }
    Ok(())
}

/// Revisa el contenido de una migración nueva.
pub fn revisar_migracion(nombre: &str, contenido: &str, permisos: Permisos) -> Result<(), String> {
    // Los comentarios no cuentan: pueden explicar lo que no se hace.
    let sin_comentarios: String =
        contenido.lines().map(|r| r.split("--").next().unwrap_or_default()).collect::<Vec<_>>().join("\n");
    if !permisos.borra_datos && BORRA.is_match(&sin_comentarios) {
        return Err(format!(
            "La migración {nombre} borra datos o estructura, y esta mejora no tenía permiso para eso."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn cambio(estado: &str, ruta: &str) -> Cambio {
        Cambio { estado: estado.into(), ruta: ruta.into() }
    }

    #[test]
    fn lee_lo_que_informa_git() {
        let estado = " M crates/servicio/src/buscar.rs\n?? crates/servicio/tests/nueva.rs\nA  migraciones/0004_x.sql\nR  mod/a.ts -> mod/b.ts\n D bateria/inicial.json\n";
        assert_eq!(
            leer_cambios(estado),
            vec![
                cambio("M", "crates/servicio/src/buscar.rs"),
                cambio("??", "crates/servicio/tests/nueva.rs"),
                cambio("A", "migraciones/0004_x.sql"),
                cambio("R", "mod/a.ts"),
                cambio("R", "mod/b.ts"),
                cambio("D", "bateria/inicial.json"),
            ]
        );
        assert_eq!(leer_cambios(""), Vec::<Cambio>::new());
    }

    #[test]
    fn solo_deja_tocar_el_servicio() {
        let sin = Permisos::default();
        let servicio = [cambio("M", "crates/servicio/src/estado.rs"), cambio("??", "crates/servicio/tests/nueva.rs")];
        assert_eq!(revisar_rutas(&servicio, sin), Ok(()));
        for protegido in [
            "crates/guardian/src/mejora.rs",
            "bateria/inicial.json",
            "instalacion/instalar.sh",
            "mod/hooks/register.ts",
            "Cargo.toml",
            ".sqlx/query-abc.json",
            ".env",
            "docs/diseno.md",
        ] {
            let motivo = revisar_rutas(
                &[cambio("M", protegido)],
                Permisos { toca_base: true, borra_datos: true, suma_librerias: true },
            )
            .unwrap_err();
            assert!(motivo.contains("no puede tocar") && motivo.contains(protegido), "{motivo}");
        }
    }

    #[test]
    fn las_librerias_y_la_base_piden_permiso() {
        let sin = Permisos::default();
        let librerias = [cambio("M", "Cargo.lock"), cambio("M", "crates/servicio/Cargo.toml")];
        assert!(revisar_rutas(&librerias, sin).unwrap_err().contains("librerías"));
        assert_eq!(revisar_rutas(&librerias, Permisos { suma_librerias: true, ..sin }), Ok(()));

        let nueva = [cambio("??", "migraciones/0004_indice.sql")];
        assert!(revisar_rutas(&nueva, sin).unwrap_err().contains("estructura de la base"));
        assert_eq!(revisar_rutas(&nueva, Permisos { toca_base: true, ..sin }), Ok(()));
        // Una migración ya aplicada no se toca ni con permiso.
        let vieja = [cambio("M", "migraciones/0001_inicial.sql")];
        let todo = Permisos { toca_base: true, borra_datos: true, suma_librerias: true };
        assert!(revisar_rutas(&vieja, todo).unwrap_err().contains("ya estaba aplicada"));
    }

    #[test]
    fn una_migracion_no_borra_sin_permiso() {
        let sin = Permisos { toca_base: true, ..Permisos::default() };
        assert_eq!(
            revisar_migracion(
                "0004_x.sql",
                "create index on notas (creada);\nalter table notas add column x int;",
                sin
            ),
            Ok(())
        );
        assert_eq!(
            revisar_migracion("0004_x.sql", "-- no hace falta drop table\ncreate table t (id int);", sin),
            Ok(())
        );
        for mala in
            ["DROP TABLE notas;", "alter table notas drop column cuerpo;", "delete from notas;", "truncate fragmentos;"]
        {
            assert!(revisar_migracion("0004_x.sql", mala, sin).unwrap_err().contains("borra datos"), "{mala}");
            assert_eq!(revisar_migracion("0004_x.sql", mala, Permisos { borra_datos: true, ..sin }), Ok(()));
        }
    }
}
