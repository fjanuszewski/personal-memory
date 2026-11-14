//! Mejoras (punto 4.8 del diseño): el reporte de una falla, la pregunta al
//! usuario y lo que pasa después. El servicio solo lleva la cuenta; el trabajo
//! (copia, agente, batería, instalación) lo hace el guardián, que es otro
//! programa y mira esta misma tabla.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::base;
use crate::error::{Resultado, uso};
use crate::notas::buscar_secreto;
use crate::texto;

const LARGO_RESUMEN: usize = 400;
const LARGO_DETALLE: usize = 4000;
/// Un aviso que nadie vio en su conversación pasa, después de este rato, a
/// la próxima conversación del mismo grupo.
const HORAS_PARA_EL_GRUPO: i64 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstadoMejora {
    /// Registrada: falta preguntarle al usuario.
    Propuesta,
    /// El usuario dijo que sí: espera al guardián.
    Aceptada,
    /// El guardián la está trabajando.
    EnCurso,
    Instalada,
    /// No se pudo, o lo que hizo el agente no pasó las pruebas: todo quedó como estaba.
    Fallida,
    /// El usuario dijo que no.
    Rechazada,
}

impl EstadoMejora {
    #[must_use]
    pub const fn clave(self) -> &'static str {
        match self {
            Self::Propuesta => "propuesta",
            Self::Aceptada => "aceptada",
            Self::EnCurso => "en_curso",
            Self::Instalada => "instalada",
            Self::Fallida => "fallida",
            Self::Rechazada => "rechazada",
        }
    }

    /// Si ya no va a cambiar sola.
    #[must_use]
    pub const fn terminada(self) -> bool {
        matches!(self, Self::Instalada | Self::Fallida | Self::Rechazada)
    }
}

impl fmt::Display for EstadoMejora {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EnCurso => "en curso",
            otro => otro.clave(),
        })
    }
}

impl FromStr for EstadoMejora {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [Self::Propuesta, Self::Aceptada, Self::EnCurso, Self::Instalada, Self::Fallida, Self::Rechazada]
            .into_iter()
            .find(|e| e.clave() == s)
            .ok_or_else(|| format!("estado de mejora desconocido: {s}"))
    }
}

#[derive(Clone, Debug)]
pub struct Mejora {
    pub id: i64,
    pub resumen: String,
    pub estado: EstadoMejora,
    pub motivo: Option<String>,
    pub cambios: Option<String>,
    pub conversacion: Option<String>,
    pub ejemplo: Value,
    pub creada: DateTime<Utc>,
    pub empezada: Option<DateTime<Utc>>,
    pub terminada: Option<DateTime<Utc>>,
}

/// Lo que se sabe de una falla al registrarla.
#[derive(Clone, Debug, Default)]
pub struct Reporte {
    /// Una o dos oraciones: lo que se le va a preguntar al usuario.
    pub resumen: String,
    /// Qué falló, con el ejemplo concreto.
    pub detalle: String,
    pub buscado: Option<String>,
    pub esperado: Option<String>,
    pub obtenido: Option<String>,
    /// Las notas que tendrían que haber aparecido al buscar `buscado`.
    pub notas_esperadas: Vec<i64>,
    /// Lo que el arreglo puede llegar a necesitar; el usuario lo acepta o no al contestar.
    pub toca_base: bool,
    pub borra_datos: bool,
    pub suma_librerias: bool,
    /// `claude` o `servicio`.
    pub origen: &'static str,
    pub conversacion: Option<String>,
    pub grupo_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registrada {
    Nueva(i64),
    /// Ya estaba registrada y sigue esperando la respuesta del usuario.
    YaPropuesta(i64),
    /// Ya está aceptada o en curso.
    EnCurso(i64),
    /// El usuario ya dijo que no a este mismo problema.
    YaRechazada(i64),
}

fn limpio(texto: Option<&str>) -> Option<&str> {
    texto.map(str::trim).filter(|t| !t.is_empty())
}

/// Las columnas de una mejora, en el orden en que se piden siempre.
const COLUMNAS: &str = "id, resumen, estado, motivo, cambios, conversacion, ejemplo, creada, empezada, terminada";

#[derive(sqlx::FromRow)]
struct Fila {
    id: i64,
    resumen: String,
    estado: String,
    motivo: Option<String>,
    cambios: Option<String>,
    conversacion: Option<String>,
    /// Texto JSON.
    ejemplo: String,
    creada: DateTime<Utc>,
    empezada: Option<DateTime<Utc>>,
    terminada: Option<DateTime<Utc>>,
}

impl From<Fila> for Mejora {
    fn from(f: Fila) -> Self {
        Self {
            id: f.id,
            resumen: f.resumen,
            estado: f.estado.parse().unwrap_or(EstadoMejora::Propuesta),
            motivo: f.motivo,
            cambios: f.cambios,
            conversacion: f.conversacion,
            ejemplo: serde_json::from_str(&f.ejemplo).unwrap_or(Value::Null),
            creada: f.creada,
            empezada: f.empezada,
            terminada: f.terminada,
        }
    }
}

/// Registra una falla. Si el mismo problema ya estaba (misma firma), avisa en
/// qué quedó en vez de registrarlo de nuevo.
pub async fn reportar(pool: &SqlitePool, r: &Reporte) -> Resultado<Registrada> {
    let resumen = r.resumen.split_whitespace().collect::<Vec<_>>().join(" ");
    let detalle = r.detalle.trim();
    if resumen.is_empty() || detalle.is_empty() {
        return uso("El reporte necesita un resumen (lo que se le pregunta al usuario) y el detalle de qué falló.");
    }
    if resumen.chars().count() > LARGO_RESUMEN {
        return uso(format!("El resumen es largo: una o dos oraciones, hasta {LARGO_RESUMEN} caracteres."));
    }
    if detalle.chars().count() > LARGO_DETALLE {
        return uso(format!("El detalle pasa de {LARGO_DETALLE} caracteres: dejá el ejemplo concreto y lo esencial."));
    }
    let buscado = limpio(r.buscado.as_deref());
    let esperado = limpio(r.esperado.as_deref());
    let obtenido = limpio(r.obtenido.as_deref());
    let todo = [Some(resumen.as_str()), Some(detalle), buscado, esperado, obtenido]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(secreto) = buscar_secreto(&todo) {
        return uso(format!("No lo registré: parece tener {secreto}. Sacalo del reporte y probá de nuevo."));
    }
    if !r.notas_esperadas.is_empty() {
        let existen: Vec<i64> =
            sqlx::query_scalar("select id from notas where id in (select value from json_each(?1))")
                .bind(base::a_json(&r.notas_esperadas))
                .fetch_all(pool)
                .await?;
        if let Some(falta) = r.notas_esperadas.iter().find(|id| !existen.contains(id)) {
            return uso(format!("No existe la nota {falta} que pusiste como esperada."));
        }
    }

    // La firma son las palabras de lo que se buscó (o del resumen), sin
    // acentos ni terminaciones: así el mismo problema se reconoce aunque se
    // escriba distinto.
    let base_de_la_firma = buscado.unwrap_or(&resumen);
    let firma = Some(texto::Consulta::de(base_de_la_firma).firma())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| texto::llano(base_de_la_firma));
    let mut tx = base::escribir(pool).await?;
    let anterior: Option<(i64, String)> = sqlx::query_as(
        "select id, estado from mejoras
         where firma = ?1 and estado in ('propuesta', 'aceptada', 'en_curso', 'rechazada')
         order by id desc limit 1",
    )
    .bind(&firma)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, estado)) = anterior {
        return Ok(match estado.as_str() {
            "propuesta" => Registrada::YaPropuesta(id),
            "rechazada" => Registrada::YaRechazada(id),
            _ => Registrada::EnCurso(id),
        });
    }

    let ejemplo = json!({
        "buscado": buscado,
        "esperado": esperado,
        "obtenido": obtenido,
        "notas_esperadas": r.notas_esperadas,
    });
    let id: i64 = sqlx::query_scalar(
        "insert into mejoras (resumen, detalle, firma, origen, conversacion, grupo_id, ejemplo,
                              toca_base, borra_datos, suma_librerias, creada)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         returning id",
    )
    .bind(&resumen)
    .bind(detalle)
    .bind(&firma)
    .bind(r.origen)
    .bind(r.conversacion.as_deref())
    .bind(r.grupo_id)
    .bind(ejemplo.to_string())
    .bind(r.toca_base)
    .bind(r.borra_datos)
    .bind(r.suma_librerias)
    .bind(base::ahora())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Registrada::Nueva(id))
}

pub async fn buscar(pool: &SqlitePool, id: i64) -> Resultado<Option<Mejora>> {
    let fila: Option<Fila> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("select {COLUMNAS} from mejoras where id = ?1")))
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(fila.map(Mejora::from))
}

/// El usuario dijo que sí: la mejora queda esperando al guardián. `conversacion` es
/// donde lo dijo, que es donde se va a avisar el resultado. También sirve para
/// volver a intentar una que falló o retomar una rechazada.
pub async fn aceptar(pool: &SqlitePool, id: i64, conversacion: Option<&str>) -> Resultado<Mejora> {
    let fila: Option<Fila> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "update mejoras set
           estado = 'aceptada', motivo = null, cambios = null, terminada = null, empezada = null,
           avisada = 0, conversacion = coalesce(?2, conversacion)
         where id = ?1 and estado in ('propuesta', 'rechazada', 'fallida')
         returning {COLUMNAS}"
    )))
    .bind(id)
    .bind(conversacion)
    .fetch_optional(pool)
    .await?;
    if let Some(fila) = fila {
        return Ok(fila.into());
    }
    match buscar(pool, id).await? {
        None => uso(format!("No existe la mejora {id}.")),
        Some(m) => uso(format!("La mejora {id} ya está {}: no hace falta aceptarla de nuevo.", m.estado)),
    }
}

/// El usuario dijo que no: no se vuelve a preguntar por el mismo problema.
pub async fn rechazar(pool: &SqlitePool, id: i64, motivo: Option<&str>) -> Resultado<Mejora> {
    let fila: Option<Fila> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "update mejoras set estado = 'rechazada', motivo = ?2, terminada = ?3, avisada = 1
         where id = ?1 and estado = 'propuesta'
         returning {COLUMNAS}"
    )))
    .bind(id)
    .bind(limpio(motivo))
    .bind(base::ahora())
    .fetch_optional(pool)
    .await?;
    if let Some(fila) = fila {
        return Ok(fila.into());
    }
    match buscar(pool, id).await? {
        None => uso(format!("No existe la mejora {id}.")),
        Some(m) => uso(format!("La mejora {id} ya está {}: no se puede rechazar ahora.", m.estado)),
    }
}

/// Lo que el mod tiene que saber de una conversación: qué mejoras suyas
/// siguen andando y cuáles terminaron sin que se haya avisado.
#[derive(Clone, Debug, Default)]
pub struct Avisos {
    pub en_curso: Vec<Mejora>,
    pub terminadas: Vec<Mejora>,
}

/// Marca como avisadas las mejoras de `vistos` (el mod ya las mostró) y
/// devuelve lo pendiente para la conversación `sesion`. Un aviso que quedó
/// sin mostrar en una conversación que no volvió a abrirse aparece, pasadas
/// unas horas, en otra del mismo grupo.
pub async fn avisos(pool: &SqlitePool, sesion: &str, vistos: &[i64]) -> Resultado<Avisos> {
    if !vistos.is_empty() {
        sqlx::query(
            "update mejoras set avisada = 1
             where id in (select value from json_each(?1)) and estado in ('instalada', 'fallida')",
        )
        .bind(base::a_json(vistos))
        .execute(pool)
        .await?;
    }
    let filas: Vec<Fila> = sqlx::query_as(
        "with esta as (
           select id, grupo_id from conversaciones where id = ?1 or transcripcion = ?1 limit 1
         )
         select m.id, m.resumen, m.estado, m.motivo, m.cambios, m.conversacion, m.ejemplo,
                m.creada, m.empezada, m.terminada
         from mejoras m, esta
         where (m.estado in ('aceptada', 'en_curso') and m.conversacion = esta.id)
            or (m.estado in ('instalada', 'fallida') and m.avisada = 0
                and (m.conversacion = esta.id
                     or (m.terminada < ?2
                         and exists (select 1 from conversaciones c
                                     where c.id = m.conversacion and c.grupo_id = esta.grupo_id))))
         order by m.id",
    )
    .bind(sesion.trim())
    .bind(base::ahora() - Duration::hours(HORAS_PARA_EL_GRUPO))
    .fetch_all(pool)
    .await?;
    let mut avisos = Avisos::default();
    for mejora in filas.into_iter().map(Mejora::from) {
        if mejora.estado.terminada() {
            avisos.terminadas.push(mejora);
        } else {
            avisos.en_curso.push(mejora);
        }
    }
    Ok(avisos)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn lee_y_escribe_los_estados() {
        for estado in ["propuesta", "aceptada", "en_curso", "instalada", "fallida", "rechazada"] {
            assert_eq!(estado.parse::<EstadoMejora>().unwrap().clave(), estado);
        }
        assert!("otra".parse::<EstadoMejora>().is_err());
        assert_eq!(EstadoMejora::EnCurso.to_string(), "en curso");
        assert!(EstadoMejora::Fallida.terminada() && !EstadoMejora::Aceptada.terminada());
    }
}
