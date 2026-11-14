//! Grupos (empresa › proyecto) y las conversaciones que caen en cada uno.
//!
//! De qué grupo es una conversación se decide acá, con lo que el mod sabe de
//! ella. No hay nombres de empresas ni de proyectos escritos en el programa:
//! salen de cómo cada uno ordena sus conversaciones.
//!
//! 1. Si la conversación está en un grupo del costado de la app, ese nombre
//!    manda: «[Empresa] - Proyecto» es esa empresa y ese proyecto, «[Empresa]»
//!    es la empresa en sí, y cualquier otro nombre va como proyecto de «Otras».
//! 2. Si no está en ningún grupo del costado, la carpeta: va al grupo donde
//!    se trabajó la última vez en esa carpeta (o en la que la contiene).
//! 3. Si esa carpeta nunca se vio, a «Otras › nombre de la carpeta».
//!
//! Los nombres se guardan como se escribieron la primera vez; después se
//! reconocen aunque cambien las mayúsculas.

use std::collections::HashSet;
use std::path::Path;

use schemars::JsonSchema;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::base;
use crate::error::{Resultado, uso};
use crate::estado::Estado;
use crate::indice::Clase;

/// Donde va lo que no es de ninguna empresa.
pub const OTRAS: &str = "Otras";
/// El proyecto que quiere decir «la empresa en sí».
pub const GENERAL: &str = "General";
/// El proyecto de una conversación de la que no se sabe ni la carpeta.
pub const SIN_GRUPO: &str = "Sin grupo";
/// Cuántas carpetas hacia arriba se mira para reconocer una carpeta ya vista.
const NIVELES: usize = 3;
/// Una carpeta con menos partes que esto (la raíz, o la carpeta personal) no
/// dice nada del grupo.
const PARTES_MINIMAS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grupo {
    pub id: i64,
    pub empresa: String,
    /// `None`: la empresa en sí.
    pub proyecto: Option<String>,
}

impl Grupo {
    /// Cómo se lee: «Empresa › Proyecto», o «Empresa» para la empresa en sí.
    #[must_use]
    pub fn nombre(&self) -> String {
        nombre(&self.empresa, self.proyecto.as_deref())
    }
}

#[must_use]
pub fn nombre(empresa: &str, proyecto: Option<&str>) -> String {
    match proyecto {
        Some(p) => format!("{empresa} › {p}"),
        None => empresa.to_owned(),
    }
}

/// Lo que el mod sabe de la conversación al empezar.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct Registro {
    /// La empresa, si ya se sabe. Sin esto, el grupo sale del nombre del
    /// costado de la app o de la carpeta.
    #[serde(default)]
    pub empresa: Option<String>,
    /// Proyecto dentro de la empresa; vacío o «General» es la empresa en sí.
    #[serde(default)]
    pub proyecto: Option<String>,
    /// Nombre del grupo en el costado de la app, como «[Empresa] - Proyecto».
    #[serde(default)]
    pub grupo_app: Option<String>,
    /// Carpeta de trabajo de la conversación.
    #[serde(default)]
    pub carpeta: Option<String>,
    /// Título de la conversación.
    #[serde(default)]
    pub titulo: Option<String>,
    /// Id del archivo de la conversación (`~/.claude/projects/…/<id>.jsonl`).
    #[serde(default)]
    pub transcripcion: Option<String>,
}

fn limpio(texto: Option<&str>) -> Option<&str> {
    texto.map(str::trim).filter(|t| !t.is_empty())
}

/// Deja empresa y proyecto prolijos: sin espacios de más, y con «General» o
/// vacío como la empresa en sí.
#[must_use]
pub fn normalizar(empresa: &str, proyecto: Option<&str>) -> (String, Option<String>) {
    let proyecto = limpio(proyecto).filter(|p| !p.eq_ignore_ascii_case(GENERAL)).map(str::to_owned);
    (empresa.trim().to_owned(), proyecto)
}

/// «[Empresa] - Proyecto» o «[Empresa]», si el texto tiene esa forma.
fn entre_corchetes(texto: &str) -> Option<(String, Option<String>)> {
    let (empresa, resto) = texto.strip_prefix('[').and_then(|r| r.split_once(']'))?;
    if empresa.trim().is_empty() {
        return None;
    }
    let proyecto = resto.trim().trim_start_matches(['-', '–', '—', ':']).trim();
    Some(normalizar(empresa, Some(proyecto)))
}

/// El grupo que corresponde a un nombre del costado de la app: con forma de
/// «[Empresa] - Proyecto», esa empresa y ese proyecto; cualquier otro nombre,
/// un proyecto de «Otras».
#[must_use]
pub fn de_grupo_app(nombre: &str) -> Option<(String, Option<String>)> {
    let t = nombre.trim();
    if t.is_empty() {
        return None;
    }
    entre_corchetes(t).or_else(|| Some((OTRAS.to_owned(), Some(t.to_owned()))))
}

/// Lee un grupo escrito a mano: «Empresa › Proyecto», «Empresa > Proyecto»,
/// «Empresa/Proyecto», «[Empresa] - Proyecto» o solo «Empresa».
#[must_use]
pub fn leer_nombre(texto: &str) -> Option<(String, Option<String>)> {
    let t = texto.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(par) = entre_corchetes(t) {
        return Some(par);
    }
    for separador in ["›", ">", "/", " - ", "–", "—", ":"] {
        if let Some((empresa, proyecto)) = t.split_once(separador) {
            return Some(normalizar(empresa, Some(proyecto)));
        }
    }
    Some(normalizar(t, None))
}

/// El grupo de una conversación de la que solo se sabe la carpeta, cuando
/// esa carpeta no se vio nunca: «Otras › nombre de la carpeta».
#[must_use]
pub fn por_defecto(carpeta: Option<&str>) -> (String, Option<String>) {
    let nombre = carpeta
        .and_then(|c| Path::new(c).file_name())
        .and_then(|n| n.to_str())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(SIN_GRUPO);
    (OTRAS.to_owned(), Some(nombre.to_owned()))
}

type Fila = (i64, String, String);

fn grupo((id, empresa, proyecto): Fila) -> Grupo {
    Grupo { id, empresa, proyecto: (!proyecto.is_empty()).then_some(proyecto) }
}

/// Si dos nombres son el mismo sin mirar mayúsculas (con acentos incluidos).
fn mismo(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

pub async fn todos(pool: &SqlitePool) -> Resultado<Vec<Grupo>> {
    let filas: Vec<Fila> = sqlx::query_as("select id, empresa, proyecto from grupos order by empresa, proyecto, id")
        .fetch_all(pool)
        .await?;
    Ok(filas.into_iter().map(grupo).collect())
}

pub async fn existente(pool: &SqlitePool, empresa: &str, proyecto: Option<&str>) -> Resultado<Option<Grupo>> {
    // Los grupos son pocos: se comparan acá, que entiende de mayúsculas con acento.
    let mut grupos = todos(pool).await?;
    grupos.sort_by_key(|g| g.id);
    Ok(grupos.into_iter().find(|g| {
        mismo(&g.empresa, empresa)
            && match (g.proyecto.as_deref(), proyecto) {
                (Some(a), Some(b)) => mismo(a, b),
                (None, None) => true,
                _ => false,
            }
    }))
}

pub async fn obtener_o_crear(pool: &SqlitePool, empresa: &str, proyecto: Option<&str>) -> Resultado<Grupo> {
    let (empresa, proyecto) = normalizar(empresa, proyecto);
    if empresa.is_empty() {
        return uso("falta la empresa del grupo");
    }
    if let Some(g) = existente(pool, &empresa, proyecto.as_deref()).await? {
        return Ok(g);
    }
    let fila: Fila = sqlx::query_as(
        "insert into grupos (empresa, proyecto) values (?1, ?2)
         on conflict (empresa, proyecto) do update set empresa = excluded.empresa
         returning id, empresa, proyecto",
    )
    .bind(&empresa)
    .bind(proyecto.as_deref().unwrap_or_default())
    .fetch_one(pool)
    .await?;
    Ok(grupo(fila))
}

async fn por_grupo_app(pool: &SqlitePool, nombre: &str) -> Resultado<Option<Grupo>> {
    let filas: Vec<(i64, String, String, String)> =
        sqlx::query_as("select id, empresa, proyecto, grupos_app from grupos order by id").fetch_all(pool).await?;
    Ok(filas
        .into_iter()
        .find(|(_, _, _, del_costado)| base::lista(del_costado).iter().any(|a| mismo(a, nombre)))
        .map(|(id, empresa, proyecto, _)| grupo((id, empresa, proyecto))))
}

/// El grupo donde se trabajó la última vez en `carpeta` o, si ahí nunca se
/// trabajó, en alguna de las que la contienen. No cuenta la propia conversación.
async fn de_carpeta(pool: &SqlitePool, carpeta: &str, salvo: &str) -> Resultado<Option<Grupo>> {
    let mut actual = Path::new(carpeta);
    for _ in 0..=NIVELES {
        if actual.components().count() < PARTES_MINIMAS {
            break;
        }
        let Some(texto) = actual.to_str() else { break };
        let fila: Option<Fila> = sqlx::query_as(
            "select g.id, g.empresa, g.proyecto
             from conversaciones c join grupos g on g.id = c.grupo_id
             where c.carpeta = ?1 and c.id <> ?2
             order by c.actualizada desc limit 1",
        )
        .bind(texto)
        .bind(salvo)
        .fetch_optional(pool)
        .await?;
        if let Some(fila) = fila {
            return Ok(Some(grupo(fila)));
        }
        let Some(arriba) = actual.parent() else { break };
        actual = arriba;
    }
    Ok(None)
}

/// De qué grupo es una conversación, con lo que el mod sabe de ella (ver la
/// regla al principio de este archivo).
async fn clasificar(pool: &SqlitePool, sesion: &str, r: &Registro) -> Resultado<Grupo> {
    if let Some(empresa) = limpio(r.empresa.as_deref()) {
        return obtener_o_crear(pool, empresa, r.proyecto.as_deref()).await;
    }
    if let Some((empresa, proyecto)) = r.grupo_app.as_deref().and_then(de_grupo_app) {
        return obtener_o_crear(pool, &empresa, proyecto.as_deref()).await;
    }
    // Sin grupo del costado. Si la conversación ya estaba registrada se queda
    // donde está: puede ser solo que la app todavía no respondió.
    if let Some(actual) = de_conversacion(pool, sesion).await? {
        return Ok(actual);
    }
    let carpeta = limpio(r.carpeta.as_deref()).filter(|c| *c != "/");
    if let Some(carpeta) = carpeta
        && let Some(conocido) = de_carpeta(pool, carpeta, sesion).await?
    {
        return Ok(conocido);
    }
    let (empresa, proyecto) = por_defecto(carpeta);
    obtener_o_crear(pool, &empresa, proyecto.as_deref()).await
}

/// Guarda la conversación con su grupo y suma la carpeta y el nombre del
/// costado al grupo, para reconocerlos después. Si la conversación pasó a
/// otro archivo (por ejemplo, después de un `/clear`), se lee desde el
/// principio.
pub async fn registrar(e: &Estado, sesion: &str, r: &Registro) -> Resultado<Grupo> {
    let sesion = sesion.trim();
    if sesion.is_empty() {
        return uso("falta el id de la conversación");
    }
    let grupo = clasificar(&e.pool, sesion, r).await?;
    let carpeta = limpio(r.carpeta.as_deref()).filter(|c| *c != "/");
    let grupo_app = limpio(r.grupo_app.as_deref());
    let uno = |texto: Option<&str>| texto.map(str::to_owned).into_iter().collect::<Vec<String>>();

    let mut tx = base::escribir(&e.pool).await?;
    let (carpetas, del_costado): (String, String) =
        sqlx::query_as("select carpetas, grupos_app from grupos where id = ?1")
            .bind(grupo.id)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("update grupos set carpetas = ?2, grupos_app = ?3 where id = ?1")
        .bind(grupo.id)
        .bind(base::a_json(&base::unir(&base::lista(&carpetas), &uno(carpeta))))
        .bind(base::a_json(&base::unir(&base::lista(&del_costado), &uno(grupo_app))))
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "insert into conversaciones (id, transcripcion, titulo, carpeta, grupo_id, actualizada)
         values (?1, ?2, ?3, ?4, ?5, ?6)
         on conflict (id) do update set
           leida_hasta = case when excluded.transcripcion <> conversaciones.transcripcion
                              then 0 else conversaciones.leida_hasta end,
           transcripcion = coalesce(excluded.transcripcion, conversaciones.transcripcion),
           titulo = coalesce(excluded.titulo, conversaciones.titulo),
           carpeta = coalesce(excluded.carpeta, conversaciones.carpeta),
           grupo_id = excluded.grupo_id,
           actualizada = excluded.actualizada",
    )
    .bind(sesion)
    .bind(limpio(r.transcripcion.as_deref()))
    .bind(limpio(r.titulo.as_deref()))
    .bind(carpeta)
    .bind(grupo.id)
    .bind(base::ahora())
    .execute(&mut *tx)
    .await?;
    // Si la conversación cambió de grupo, sus pedazos ya leídos van con ella.
    let movidos: Vec<i64> = sqlx::query_scalar(
        "update fragmentos set grupo_id = ?2 where conversacion = ?1 and grupo_id is not ?2 returning id",
    )
    .bind(sesion)
    .bind(grupo.id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    if !movidos.is_empty() {
        let movidos: HashSet<i64> = movidos.into_iter().collect();
        e.indice.mover(Clase::Fragmentos, &|id| movidos.contains(&id), Some(grupo.id));
    }
    Ok(grupo)
}

/// El grupo de una conversación registrada, por su id de la app o por el de
/// su archivo.
pub async fn de_conversacion(pool: &SqlitePool, sesion: &str) -> Resultado<Option<Grupo>> {
    let fila: Option<Fila> = sqlx::query_as(
        "select g.id, g.empresa, g.proyecto
         from conversaciones c join grupos g on g.id = c.grupo_id
         where c.id = ?1 or c.transcripcion = ?1
         order by c.actualizada desc limit 1",
    )
    .bind(sesion.trim())
    .fetch_optional(pool)
    .await?;
    Ok(fila.map(grupo))
}

/// El id con que está guardada la conversación, si se conoce.
pub async fn conversacion_conocida(pool: &SqlitePool, sesion: &str) -> Resultado<Option<String>> {
    Ok(sqlx::query_scalar("select id from conversaciones where id = ?1 or transcripcion = ?1 limit 1")
        .bind(sesion.trim())
        .fetch_optional(pool)
        .await?)
}

/// Un grupo escrito por Claude: primero por el nombre del costado, después
/// por empresa › proyecto. Con `crear`, uno nuevo si no existe.
pub async fn por_nombre(pool: &SqlitePool, texto: &str, crear: bool) -> Resultado<Grupo> {
    if let Some(g) = por_grupo_app(pool, texto.trim()).await? {
        return Ok(g);
    }
    let Some((empresa, proyecto)) = leer_nombre(texto) else {
        return uso("el grupo está vacío");
    };
    if let Some(g) = existente(pool, &empresa, proyecto.as_deref()).await? {
        return Ok(g);
    }
    if proyecto.is_none()
        && let Some(g) = existente(pool, OTRAS, Some(texto.trim())).await?
    {
        return Ok(g);
    }
    if crear {
        return obtener_o_crear(pool, &empresa, proyecto.as_deref()).await;
    }
    let conocidos: Vec<String> = todos(pool).await?.iter().map(Grupo::nombre).collect();
    uso(format!(
        "No conozco el grupo «{}». Grupos conocidos: {}.",
        texto.trim(),
        if conocidos.is_empty() { "ninguno todavía".to_owned() } else { conocidos.join(", ") }
    ))
}

/// El grupo que corresponde a un pedido: el que se nombra, o el de la
/// conversación. `None` si no hay ninguno de los dos.
pub async fn resolver(
    pool: &SqlitePool,
    sesion: Option<&str>,
    grupo: Option<&str>,
    crear: bool,
) -> Resultado<Option<Grupo>> {
    if let Some(texto) = limpio(grupo) {
        return por_nombre(pool, texto, crear).await.map(Some);
    }
    let Some(sesion) = limpio(sesion) else {
        return Ok(None);
    };
    match de_conversacion(pool, sesion).await? {
        Some(g) => Ok(Some(g)),
        None => uso(format!("No conozco la conversación «{sesion}». Pasá `grupo` (por ejemplo «Empresa › Proyecto»).")),
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn par(e: &str, p: Option<&str>) -> (String, Option<String>) {
        (e.to_owned(), p.map(str::to_owned))
    }

    #[test]
    fn deja_prolijos_empresa_y_proyecto() {
        assert_eq!(normalizar(" acme ", Some(" PORTAL ")), par("acme", Some("PORTAL")));
        assert_eq!(normalizar("Globex", Some("General")), par("Globex", None));
        assert_eq!(normalizar("Globex", Some("general")), par("Globex", None));
        assert_eq!(normalizar("Otras", Some("")), par("Otras", None));
        assert_eq!(normalizar("Otras", None), par("Otras", None));
    }

    #[test]
    fn lee_grupos_escritos_de_varias_formas() {
        assert_eq!(leer_nombre("Acme › Portal"), Some(par("Acme", Some("Portal"))));
        assert_eq!(leer_nombre("acme > tienda"), Some(par("acme", Some("tienda"))));
        assert_eq!(leer_nombre("Acme/Agenda"), Some(par("Acme", Some("Agenda"))));
        assert_eq!(leer_nombre("[ACME] - PORTAL"), Some(par("ACME", Some("PORTAL"))));
        assert_eq!(leer_nombre("[GLOBEX]"), Some(par("GLOBEX", None)));
        assert_eq!(leer_nombre("Initech - Nomina"), Some(par("Initech", Some("Nomina"))));
        assert_eq!(leer_nombre("Globex"), Some(par("Globex", None)));
        assert_eq!(leer_nombre("  "), None);
    }

    #[test]
    fn el_nombre_del_costado_dice_el_grupo() {
        assert_eq!(de_grupo_app("[ACME] - PORTAL"), Some(par("ACME", Some("PORTAL"))));
        assert_eq!(de_grupo_app("[Acme]: Tienda"), Some(par("Acme", Some("Tienda"))));
        assert_eq!(de_grupo_app(" [GLOBEX] "), Some(par("GLOBEX", None)));
        // Sin corchetes es un proyecto suelto, aunque tenga un guion.
        assert_eq!(de_grupo_app("Cosas de casa"), Some(par(OTRAS, Some("Cosas de casa"))));
        assert_eq!(de_grupo_app("Viaje - marzo"), Some(par(OTRAS, Some("Viaje - marzo"))));
        assert_eq!(de_grupo_app("[] - algo"), Some(par(OTRAS, Some("[] - algo"))));
        assert_eq!(de_grupo_app("   "), None);
    }

    #[test]
    fn una_carpeta_nueva_va_con_su_nombre() {
        assert_eq!(por_defecto(Some("/Users/ana/proyectos/portal")), par(OTRAS, Some("portal")));
        assert_eq!(por_defecto(Some("/Users/ana/proyectos/portal/")), par(OTRAS, Some("portal")));
        assert_eq!(por_defecto(Some("/")), par(OTRAS, Some(SIN_GRUPO)));
        assert_eq!(por_defecto(None), par(OTRAS, Some(SIN_GRUPO)));
    }

    #[test]
    fn nombra_los_grupos() {
        assert_eq!(nombre("Acme", Some("Portal")), "Acme › Portal");
        assert_eq!(nombre("Globex", None), "Globex");
    }
}
