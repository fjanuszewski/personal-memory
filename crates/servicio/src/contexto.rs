//! El resumen corto de un grupo que Claude recibe al empezar: decisiones
//! vigentes, errores, pendientes, acuerdos y cambios recientes. Unas 30 líneas
//! como mucho.

use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};
use sqlx::SqlitePool;

use crate::error::Resultado;
use crate::grupos::{self, Grupo};
use crate::modelo::TipoNota;

const LINEAS_MAXIMAS: usize = 30;

/// Cuántas notas de cada tipo entran, en el orden en que se muestran.
const SECCIONES: [(TipoNota, &str, usize); 6] = [
    (TipoNota::Decision, "Decisiones vigentes", 8),
    (TipoNota::Pendiente, "Pendientes", 6),
    (TipoNota::Error, "Errores recientes", 5),
    (TipoNota::Acuerdo, "Acuerdos con el cliente", 4),
    (TipoNota::Cambio, "Cambios recientes", 3),
    (TipoNota::Dato, "Datos útiles", 3),
];
const DE_LA_EMPRESA: usize = 4;

struct Fila {
    id: i64,
    tipo: TipoNota,
    titulo: String,
    actualizada: DateTime<Utc>,
}

async fn vigentes(pool: &SqlitePool, grupo_id: i64) -> Resultado<Vec<Fila>> {
    let filas: Vec<(i64, String, String, DateTime<Utc>)> = sqlx::query_as(
        "select id, tipo, titulo, actualizada from notas
         where grupo_id = ?1 and estado = 'vigente'
         order by actualizada desc limit 80",
    )
    .bind(grupo_id)
    .fetch_all(pool)
    .await?;
    Ok(filas
        .into_iter()
        .map(|(id, tipo, titulo, actualizada)| Fila {
            id,
            tipo: tipo.parse().unwrap_or(TipoNota::Dato),
            titulo,
            actualizada,
        })
        .collect())
}

fn linea(f: &Fila, con_tipo: bool) -> String {
    let fecha = f.actualizada.with_timezone(&Local).format("%Y-%m-%d");
    if con_tipo {
        format!("- [nota {}] {} · {fecha} · {}", f.id, f.tipo, f.titulo)
    } else {
        format!("- [nota {}] {fecha} · {}", f.id, f.titulo)
    }
}

pub async fn resumen(pool: &SqlitePool, grupo: &Grupo) -> Resultado<String> {
    let propias = vigentes(pool, grupo.id).await?;
    let empresa = match &grupo.proyecto {
        Some(_) => match grupos::existente(pool, &grupo.empresa, None).await? {
            Some(g) => vigentes(pool, g.id).await?,
            None => Vec::new(),
        },
        None => Vec::new(),
    };

    let cabeza = format!(
        "Grupo: {} · {} {} vigente{}",
        grupo.nombre(),
        propias.len(),
        if propias.len() == 1 { "nota" } else { "notas" },
        if propias.len() == 1 { "" } else { "s" },
    );
    if propias.is_empty() && empresa.is_empty() {
        return Ok(format!("{cabeza}\nTodavía no hay notas en este grupo."));
    }

    // Cada sección: su título y sus líneas, en el orden en que se muestran.
    let mut secciones: Vec<(String, Vec<String>)> = Vec::new();
    for (tipo, titulo, cuantas) in SECCIONES {
        let de_tipo: Vec<String> =
            propias.iter().filter(|f| f.tipo == tipo).take(cuantas).map(|f| linea(f, false)).collect();
        if !de_tipo.is_empty() {
            secciones.push((format!("{titulo}:"), de_tipo));
        }
    }
    if !empresa.is_empty() {
        let de_empresa = empresa.iter().take(DE_LA_EMPRESA).map(|f| linea(f, true)).collect();
        secciones.push((format!("De {} en general:", grupo.empresa), de_empresa));
    }
    let recortado = recortar(&mut secciones);

    let mut salida = format!("{cabeza}\n");
    for (titulo, lineas) in &secciones {
        let _ = writeln!(salida, "{titulo}");
        for l in lineas {
            let _ = writeln!(salida, "{l}");
        }
    }
    if recortado {
        let _ = writeln!(salida, "(Hay más: buscalas con memoria_buscar.)");
    }
    Ok(salida.trim_end().to_owned())
}

fn total(secciones: &[(String, Vec<String>)]) -> usize {
    secciones.iter().map(|(_, l)| l.len() + 1).sum()
}

/// Deja todo en `LINEAS_MAXIMAS`, contando la cabeza y el aviso de que hay
/// más: saca de a una línea de la sección más larga, para que ninguna quede
/// afuera entera mientras se pueda.
fn recortar(secciones: &mut Vec<(String, Vec<String>)>) -> bool {
    // La cabeza ocupa una línea.
    if total(secciones) < LINEAS_MAXIMAS {
        return false;
    }
    let lugar = LINEAS_MAXIMAS - 2;
    while total(secciones) > lugar {
        let mas_larga = secciones.iter_mut().filter(|(_, l)| l.len() > 1).max_by_key(|(_, l)| l.len());
        match mas_larga {
            Some((_, l)) => {
                l.pop();
            }
            None => {
                secciones.pop();
            }
        }
    }
    true
}
