//! Mejoras (punto 4.8 del diseño): el reporte de una falla, la pregunta al
//! usuario y lo que pasa después. El servicio solo lleva la cuenta y hace de
//! buzón: el guardián (otro programa, que mira esta misma tabla) arma la
//! copia del código, revisa, prueba, mide e instala; el agente que hace el
//! cambio corre de fondo dentro de una conversación de Claude, lanzado por el
//! mod, que se entera por acá de que hay una mejora lista.

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
/// Cuántos agentes pueden estar trabajando a la vez, entre todas las conversaciones.
const AGENTES_A_LA_VEZ: i64 = 2;
const LARGO_INFORME: usize = 4000;
const LARGO_MOTIVO: usize = 600;

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

/// En qué parte del recorrido está una mejora en curso. La anotan el guardián
/// y, por medio de [`agente`], el mod de la conversación donde corre el agente.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fase {
    /// El guardián arma la copia del código.
    Preparando,
    /// Lista: espera que una conversación de Claude abierta lance al agente.
    EsperandoAgente,
    /// Un agente la trabaja, de fondo, en una conversación.
    ConAgente,
    /// El agente terminó: espera al guardián.
    AgenteListo,
    /// El agente no pudo terminar: el guardián decide si se lanza otro.
    AgenteFallo,
    /// El guardián revisa, compila, prueba, mide e instala.
    Probando,
}

impl Fase {
    const TODAS: [Self; 6] = [
        Self::Preparando,
        Self::EsperandoAgente,
        Self::ConAgente,
        Self::AgenteListo,
        Self::AgenteFallo,
        Self::Probando,
    ];

    #[must_use]
    pub const fn clave(self) -> &'static str {
        match self {
            Self::Preparando => "preparando",
            Self::EsperandoAgente => "esperando_agente",
            Self::ConAgente => "con_agente",
            Self::AgenteListo => "agente_listo",
            Self::AgenteFallo => "agente_fallo",
            Self::Probando => "probando",
        }
    }
}

impl FromStr for Fase {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::TODAS.into_iter().find(|f| f.clave() == s).ok_or_else(|| format!("fase de mejora desconocida: {s}"))
    }
}

#[derive(Clone, Debug)]
pub struct Mejora {
    pub id: i64,
    pub resumen: String,
    pub estado: EstadoMejora,
    /// Solo mientras está en curso.
    pub fase: Option<Fase>,
    pub fase_desde: Option<DateTime<Utc>>,
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
const COLUMNAS: &str =
    "id, resumen, estado, fase, fase_desde, motivo, cambios, conversacion, ejemplo, creada, empezada, terminada";

#[derive(sqlx::FromRow)]
struct Fila {
    id: i64,
    resumen: String,
    estado: String,
    fase: Option<String>,
    fase_desde: Option<DateTime<Utc>>,
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
            fase: f.fase.and_then(|fase| fase.parse().ok()),
            fase_desde: f.fase_desde,
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
           avisada = 0, conversacion = coalesce(?2, conversacion),
           fase = null, fase_desde = null, encargo = null, informe = null, desde_version = null,
           agente_sesion = null, agente_visto = null, lanzamientos = 0
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

/// Una mejora lista para que una conversación lance su agente.
#[derive(Clone, Debug, PartialEq)]
pub struct Encargo {
    pub id: i64,
    /// Lo que dejó el guardián para el mod (la carpeta de la copia, el
    /// `cargo` de la mejora y el pedido). El servicio lo pasa tal cual.
    pub encargo: Value,
}

/// Lo que el mod tiene que saber de una conversación: qué mejoras suyas
/// siguen andando, cuáles terminaron sin que se haya avisado, cuáles esperan
/// que lance un agente y cuáles tienen su agente corriendo en ella.
#[derive(Clone, Debug, Default)]
pub struct Avisos {
    pub en_curso: Vec<Mejora>,
    pub terminadas: Vec<Mejora>,
    pub para_agente: Vec<Encargo>,
    pub con_agente: Vec<i64>,
}

/// La conversación `sesion` como la conoce el servicio, con su grupo.
async fn esta_conversacion<'e, E>(conexion: E, sesion: &str) -> Resultado<Option<(String, Option<i64>)>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    Ok(sqlx::query_as("select id, grupo_id from conversaciones where id = ?1 or transcripcion = ?1 limit 1")
        .bind(sesion.trim())
        .fetch_optional(conexion)
        .await?)
}

/// A qué mejoras les puede lanzar el agente una conversación: a las que se
/// aceptaron en ella y a las que no tienen conversación; y, si la suya no
/// volvió a abrirse en unas horas, a las de otra conversación de su grupo.
const PUEDE_LANZAR: &str = "m.estado = 'en_curso' and m.fase = 'esperando_agente' and m.encargo is not null
     and (m.conversacion is null or m.conversacion = ?1
          or (m.empezada < ?2
              and exists (select 1 from conversaciones c where c.id = m.conversacion and c.grupo_id is ?3)))";

async fn agentes_trabajando<'e, E>(conexion: E) -> Resultado<i64>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    Ok(sqlx::query_scalar("select count(*) from mejoras where estado = 'en_curso' and fase = 'con_agente'")
        .fetch_one(conexion)
        .await?)
}

/// Marca como avisadas las mejoras de `vistos` (el mod ya las mostró), anota
/// que los agentes de `agentes` siguen andando en esta conversación y
/// devuelve lo pendiente para ella. Un aviso que quedó sin mostrar en una
/// conversación que no volvió a abrirse aparece, pasadas unas horas, en otra
/// del mismo grupo.
pub async fn avisos(pool: &SqlitePool, sesion: &str, vistos: &[i64], agentes: &[i64]) -> Resultado<Avisos> {
    if !vistos.is_empty() {
        sqlx::query(
            "update mejoras set avisada = 1
             where id in (select value from json_each(?1)) and estado in ('instalada', 'fallida')",
        )
        .bind(base::a_json(vistos))
        .execute(pool)
        .await?;
    }
    let Some((conversacion, grupo)) = esta_conversacion(pool, sesion).await? else {
        return Ok(Avisos::default());
    };
    let ahora = base::ahora();
    if !agentes.is_empty() {
        sqlx::query(
            "update mejoras set agente_visto = ?3
             where id in (select value from json_each(?1))
               and estado = 'en_curso' and fase = 'con_agente' and agente_sesion = ?2",
        )
        .bind(base::a_json(agentes))
        .bind(&conversacion)
        .bind(ahora)
        .execute(pool)
        .await?;
    }
    let hace_horas = ahora - Duration::hours(HORAS_PARA_EL_GRUPO);
    let filas: Vec<Fila> = sqlx::query_as(
        "select m.id, m.resumen, m.estado, m.fase, m.fase_desde, m.motivo, m.cambios, m.conversacion, m.ejemplo,
                m.creada, m.empezada, m.terminada
         from mejoras m
         where (m.estado in ('aceptada', 'en_curso') and m.conversacion = ?1)
            or (m.estado in ('instalada', 'fallida') and m.avisada = 0
                and (m.conversacion = ?1
                     or (m.terminada < ?2
                         and exists (select 1 from conversaciones c
                                     where c.id = m.conversacion and c.grupo_id is ?3))))
         order by m.id",
    )
    .bind(&conversacion)
    .bind(hace_horas)
    .bind(grupo)
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
    avisos.con_agente = sqlx::query_scalar(
        "select id from mejoras where estado = 'en_curso' and fase = 'con_agente' and agente_sesion = ?1 order by id",
    )
    .bind(&conversacion)
    .fetch_all(pool)
    .await?;
    let libres = usize::try_from(AGENTES_A_LA_VEZ - agentes_trabajando(pool).await?).unwrap_or(0);
    if libres > 0 {
        let listas: Vec<(i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select m.id, m.encargo from mejoras m where {PUEDE_LANZAR} order by m.id"
        )))
        .bind(&conversacion)
        .bind(hace_horas)
        .bind(grupo)
        .fetch_all(pool)
        .await?;
        avisos.para_agente = listas
            .into_iter()
            .filter_map(|(id, encargo)| Some(Encargo { id, encargo: serde_json::from_str(&encargo).ok()? }))
            .take(libres)
            .collect();
    }
    Ok(avisos)
}

/// Lo que el mod anota del agente de una mejora.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paso {
    /// La conversación toma la mejora: va a lanzar su agente.
    Empezo,
    /// El agente terminó; `informe` es lo último que dijo.
    Termino { informe: String },
    /// El agente no arrancó o no pudo terminar.
    Fallo { motivo: String },
}

fn recortar(texto: &str, largo: usize) -> String {
    let texto = texto.trim();
    if texto.chars().count() <= largo {
        return texto.to_owned();
    }
    format!("{}…", texto.chars().take(largo).collect::<String>().trim_end())
}

/// Anota un paso del agente de la mejora `id`, que corre en la conversación
/// `sesion`. Devuelve si el paso valió: una mejora la toma una sola
/// conversación, y solo esa puede decir que su agente terminó o falló.
pub async fn agente(pool: &SqlitePool, id: i64, sesion: &str, paso: &Paso) -> Resultado<bool> {
    let mut tx = base::escribir(pool).await?;
    let Some((conversacion, grupo)) = esta_conversacion(&mut *tx, sesion).await? else {
        return uso("No conozco esa conversación: registrala primero con memoria_contexto.");
    };
    let ahora = base::ahora();
    let cambiadas = match paso {
        Paso::Empezo => {
            if agentes_trabajando(&mut *tx).await? >= AGENTES_A_LA_VEZ {
                return Ok(false);
            }
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "update mejoras as m set
                   fase = 'con_agente', fase_desde = ?5, agente_sesion = ?1, agente_visto = ?5,
                   lanzamientos = lanzamientos + 1
                 where m.id = ?4 and {PUEDE_LANZAR}"
            )))
            .bind(&conversacion)
            .bind(ahora - Duration::hours(HORAS_PARA_EL_GRUPO))
            .bind(grupo)
            .bind(id)
            .bind(ahora)
            .execute(&mut *tx)
            .await?
        }
        Paso::Termino { informe } => {
            sqlx::query(
                "update mejoras set fase = 'agente_listo', fase_desde = ?3, agente_visto = ?3, informe = ?4
                 where id = ?1 and estado = 'en_curso' and fase = 'con_agente' and agente_sesion = ?2",
            )
            .bind(id)
            .bind(&conversacion)
            .bind(ahora)
            .bind(recortar(informe, LARGO_INFORME))
            .execute(&mut *tx)
            .await?
        }
        Paso::Fallo { motivo } => {
            sqlx::query(
                "update mejoras set fase = 'agente_fallo', fase_desde = ?3, motivo = ?4
                 where id = ?1 and estado = 'en_curso' and fase = 'con_agente' and agente_sesion = ?2",
            )
            .bind(id)
            .bind(&conversacion)
            .bind(ahora)
            .bind(recortar(motivo, LARGO_MOTIVO))
            .execute(&mut *tx)
            .await?
        }
    };
    tx.commit().await?;
    Ok(cambiadas.rows_affected() == 1)
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

    #[test]
    fn lee_y_escribe_las_fases() {
        for fase in Fase::TODAS {
            assert_eq!(fase.clave().parse::<Fase>().unwrap(), fase);
        }
        assert!("otra".parse::<Fase>().is_err());
    }

    #[test]
    fn recorta_lo_que_dice_el_agente() {
        assert_eq!(recortar("  Bajé el piso.  ", 40), "Bajé el piso.");
        assert_eq!(recortar("uno dos tres cuatro", 8), "uno dos…");
    }
}
