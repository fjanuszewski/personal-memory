//! Búsqueda híbrida (punto 4.6 del diseño): por palabras, en español y sin
//! acentos, y por significado, comparando huellas. De cada una se toman los
//! mejores candidatos y se mezclan por posición.
//!
//! Una coincidencia es firme si llega por significado o si el texto tiene
//! (casi) todas las palabras de la consulta. Compartir una palabra suelta no
//! alcanza: un pedazo de conversación así no se muestra, y una nota solo
//! aparece, marcada como parcial, cuando no hay nada mejor. Y eso vale dentro
//! del grupo de la conversación: una nota de otro grupo completa los
//! resultados solo si tiene al menos la mitad de las palabras buscadas, y
//! nunca una sola (entre todo lo guardado siempre hay alguna nota que
//! comparte una palabra con cualquier pregunta).
//!
//! Empieza por el grupo de la conversación y, si hay pocos resultados firmes,
//! amplía a la empresa y después a todo. Ordena: notas vigentes, pedazos de
//! conversación y, solo si se piden, notas reemplazadas. Una reemplazada que
//! coincide con la búsqueda lleva a la nota que vale hoy, que la muestra como
//! «antes se usaba».

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};
use sqlx::SqliteConnection;

use crate::base;
use crate::error::Resultado;
use crate::estado::Estado;
use crate::grupos::{self, Grupo};
use crate::indice::{Clase as Huellas, Dato, Indice};
use crate::modelo::{EstadoNota, TipoNota};
use crate::texto::{Consulta as Palabras, conjunto, extracto};

/// Con menos resultados firmes que esto se amplía el alcance, y se completa
/// con notas que coinciden solo en parte.
const MINIMO: usize = 3;
pub const LIMITE_POR_DEFECTO: usize = 8;
const LIMITE_MAXIMO: usize = 30;
/// Cuántos candidatos se toman de cada búsqueda antes de mezclar.
const CANDIDATOS: u16 = 50;
/// En una pregunta larga, cuántos pedazos de conversación se miran para
/// quedarse con los que tienen suficientes palabras.
const REVISADOS: u16 = 300;
/// La constante de la fusión por ranking: cada lista suma `1 / (K + puesto)`.
const K: f64 = 60.0;
/// Lo que suma, como mucho, que algo sea de hoy: lo mismo que subir unos
/// puestos. Baja a la mitad a los 30 días.
const BONO_RECIENTE: f64 = 0.001;
/// Lugares que se guardan para pedazos de conversación aunque sobren notas.
const RESERVA_FRAGMENTOS: usize = 3;
/// Cuántos reemplazos encadenados se siguen para llegar a la nota vigente.
const SALTOS_MAXIMOS: usize = 5;
/// El título pesa más que el cuerpo al ordenar las notas por palabras.
const PESO_TITULO: f64 = 2.5;
/// Hasta cuántos caracteres una nota se muestra entera, y de cuántas
/// palabras es el pedazo que se muestra si es más larga.
const NOTA_ENTERA: usize = 320;
const PALABRAS_DE_NOTA: usize = 50;
const PALABRAS_DE_FRAGMENTO: usize = 60;

#[derive(Clone, Debug)]
pub struct Consulta {
    pub texto: String,
    pub tipo: Option<TipoNota>,
    pub desde: Option<DateTime<Utc>>,
    /// Si también se buscan pedazos de conversaciones.
    pub conversaciones: bool,
    /// Si las notas reemplazadas van como resultados propios.
    pub reemplazadas: bool,
    pub limite: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Clase {
    Nota { tipo: TipoNota, estado: EstadoNota, reemplazada_por: Option<i64>, origenes: Vec<String> },
    Fragmento { conversacion: String, titulo: Option<String> },
}

#[derive(Clone, Debug)]
pub struct Hallazgo {
    pub id: i64,
    pub clase: Clase,
    pub titulo: String,
    pub extracto: String,
    pub fecha: DateTime<Utc>,
    pub grupo_id: Option<i64>,
    pub grupo: String,
    pub puntaje: f64,
    /// En qué paso del alcance apareció (0: el grupo de la conversación).
    pub etapa: usize,
    /// Notas reemplazadas por esta que coincidían con la búsqueda: id y título.
    pub antes: Vec<(i64, String)>,
    /// Coincide solo en alguna palabra suelta: puede no tener nada que ver.
    pub parcial: bool,
}

#[derive(Clone, Debug)]
pub struct Busqueda {
    pub hallazgos: Vec<Hallazgo>,
    /// Los alcances recorridos, en orden («ACME › Portal», «ACME», «todo»).
    pub recorrido: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Capa {
    Nota,
    Fragmento,
}

/// Una fila candidata, salga de la búsqueda que salga.
#[derive(Clone, Debug)]
struct Fila {
    id: i64,
    estado: EstadoNota,
    reemplazada_por: Option<i64>,
    fecha: DateTime<Utc>,
    /// Llegó por significado o tiene (casi) todas las palabras de la consulta.
    firme: bool,
    /// Llegó solo por palabras y con tan pocas de las buscadas que, viniendo
    /// de otro grupo, no alcanza ni para completar los resultados.
    suelta: bool,
}

#[derive(Clone, Debug)]
struct Candidato {
    capa: Capa,
    fila: Fila,
    puntaje: f64,
    etapa: usize,
    /// Notas reemplazadas que coincidían con la búsqueda y llevaron a esta.
    antes: Vec<i64>,
}

/// Un paso del alcance, para una de las dos capas.
#[derive(Clone, Copy, Debug)]
struct Paso {
    capa: Capa,
    /// Qué paso es (0: el primero).
    etapa: usize,
    /// Si busca solo dentro del grupo de la conversación.
    del_grupo: bool,
}

impl Candidato {
    fn orden(&self) -> u8 {
        match (self.capa, self.fila.estado) {
            (Capa::Nota, EstadoNota::Vigente) => 0,
            (Capa::Fragmento, _) => 1,
            (Capa::Nota, _) => 2,
        }
    }
}

fn limite(c: &Consulta) -> usize {
    c.limite.clamp(1, LIMITE_MAXIMO)
}

fn estado(texto: &str) -> EstadoNota {
    texto.parse().unwrap_or(EstadoNota::Vigente)
}

/// Fusión por ranking: cada lista le suma `1 / (K + puesto)` a sus elementos,
/// así lo que aparece arriba en las dos búsquedas queda primero.
fn fusionar(listas: &[&[i64]]) -> HashMap<i64, f64> {
    let mut puntos = HashMap::new();
    for lista in listas {
        let mut puesto = 0.0;
        for id in *lista {
            puesto += 1.0;
            *puntos.entry(*id).or_insert(0.0) += 1.0 / (K + puesto);
        }
    }
    puntos
}

fn bono_reciente(fecha: DateTime<Utc>, ahora: DateTime<Utc>) -> f64 {
    let dias = f64::from(i32::try_from((ahora - fecha).num_days().max(0)).unwrap_or(i32::MAX));
    BONO_RECIENTE / (1.0 + dias / 30.0)
}

/// Los grupos donde se busca, como lista para una consulta; `None`: todos.
fn lista(ids: Option<&[i64]>) -> Option<String> {
    ids.map(base::a_json)
}

/// Una nota que salió por palabras: sus datos y las raíces de su título y su cuerpo.
type NotaPorPalabras = (i64, String, Option<i64>, DateTime<Utc>, String, String);

async fn notas_por_palabras(
    conn: &mut SqliteConnection,
    ids: Option<&[i64]>,
    c: &Consulta,
    palabras: &Palabras,
) -> Resultado<Vec<Fila>> {
    let Some(buscado) = palabras.para_buscar(false) else {
        return Ok(Vec::new());
    };
    let filas: Vec<NotaPorPalabras> = sqlx::query_as(
        "select n.id, n.estado, n.reemplazada_por, n.creada, notas_texto.titulo, notas_texto.cuerpo
         from notas_texto join notas n on n.id = notas_texto.rowid
         where notas_texto match ?1
           and n.estado <> 'descartada'
           and (?2 is null or n.grupo_id in (select value from json_each(?2)))
           and (?3 is null or n.tipo = ?3)
           and (?4 is null or n.creada >= ?4)
         order by bm25(notas_texto, ?5, 1.0), n.id desc
         limit ?6",
    )
    .bind(buscado)
    .bind(lista(ids))
    .bind(c.tipo.map(TipoNota::clave))
    .bind(c.desde)
    .bind(PESO_TITULO)
    .bind(i64::from(CANDIDATOS))
    .fetch_all(conn)
    .await?;
    Ok(filas
        .into_iter()
        .map(|(id, estado_de_nota, reemplazada_por, creada, titulo, cuerpo)| {
            let raices = conjunto(&titulo).union(&conjunto(&cuerpo)).copied().collect();
            Fila {
                id,
                estado: estado(&estado_de_nota),
                reemplazada_por,
                fecha: creada,
                firme: palabras.firme(&raices),
                suelta: !palabras.completa(&raices),
            }
        })
        .collect())
}

/// Las huellas más parecidas a la consulta, de la más parecida a la menos.
/// La consulta puede traer más de una huella (la de lo que se escribió y la
/// de la misma pregunta con sus palabras equivalentes): de cada texto cuenta
/// el mejor parecido.
fn cercanas(indice: &Indice, clase: Huellas, huellas: &[Vec<f32>], entra: &dyn Fn(&Dato) -> bool) -> Vec<(Dato, f64)> {
    let mut mejores: Vec<(Dato, f64)> = Vec::new();
    for huella in huellas {
        for (dato, parecido) in indice.cercanas(clase, huella, usize::from(CANDIDATOS), entra) {
            match mejores.iter_mut().find(|(d, _)| d.id == dato.id) {
                Some((_, mejor)) => *mejor = mejor.max(parecido),
                None => mejores.push((dato, parecido)),
            }
        }
    }
    mejores.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.id.cmp(&a.0.id)));
    mejores.truncate(usize::from(CANDIDATOS));
    mejores
}

async fn notas_por_significado(
    conn: &mut SqliteConnection,
    indice: &Indice,
    ids: Option<&[i64]>,
    c: &Consulta,
    huellas: &[Vec<f32>],
    piso: f64,
) -> Resultado<Vec<Fila>> {
    if huellas.is_empty() {
        return Ok(Vec::new());
    }
    // Primero, cuáles pueden salir (por grupo, tipo y fecha); después, de
    // esas, las más parecidas según las huellas en memoria.
    let filas: Vec<(i64, String, Option<i64>, DateTime<Utc>)> = sqlx::query_as(
        "select n.id, n.estado, n.reemplazada_por, n.creada
         from notas n
         where n.estado <> 'descartada'
           and (?1 is null or n.grupo_id in (select value from json_each(?1)))
           and (?2 is null or n.tipo = ?2)
           and (?3 is null or n.creada >= ?3)",
    )
    .bind(lista(ids))
    .bind(c.tipo.map(TipoNota::clave))
    .bind(c.desde)
    .fetch_all(conn)
    .await?;
    let posibles: HashMap<i64, Fila> = filas
        .into_iter()
        .map(|(id, estado_de_nota, reemplazada_por, creada)| {
            let fila = Fila {
                id,
                estado: estado(&estado_de_nota),
                reemplazada_por,
                fecha: creada,
                firme: true,
                suelta: false,
            };
            (id, fila)
        })
        .collect();
    Ok(cercanas(indice, Huellas::Notas, huellas, &|d| posibles.contains_key(&d.id))
        .into_iter()
        .filter(|(_, parecido)| *parecido >= piso)
        .filter_map(|(dato, _)| posibles.get(&dato.id).cloned())
        .collect())
}

/// Lo que se le pide al índice de palabras para los pedazos de conversación.
/// Con hasta tres palabras, que estén todas. Con más alcanza con siete de
/// cada diez, y se arma así: a un pedazo que cumple le pueden faltar, como
/// mucho, `faltan` palabras, así que de cualquier grupo de `faltan + 1`
/// tiene al menos una. Se eligen las menos comunes para ese grupo: deja muy
/// pocos pedazos para revisar, sin perder ninguno de los que cumplen.
async fn pedido_de_fragmentos(conn: &mut SqliteConnection, palabras: &Palabras) -> Resultado<Option<String>> {
    let faltan = palabras.total() - palabras.necesarias().min(palabras.total());
    if faltan == 0 {
        return Ok(palabras.para_buscar(true));
    }
    let mut comunes: Vec<(i64, usize)> = Vec::with_capacity(palabras.total());
    for (lugar, raices) in palabras.palabras().iter().enumerate() {
        let mut en_cuantos = 0_i64;
        for raiz in raices {
            let cuenta: Option<i64> = sqlx::query_scalar("select doc from fragmentos_palabras where term = ?1")
                .bind(raiz)
                .fetch_optional(&mut *conn)
                .await?;
            en_cuantos += cuenta.unwrap_or(0);
        }
        comunes.push((en_cuantos, lugar));
    }
    comunes.sort_unstable();
    let raras: Vec<usize> = comunes.iter().take(faltan + 1).map(|(_, lugar)| *lugar).collect();
    Ok(palabras.para_buscar_con(&raras))
}

async fn fragmentos_por_palabras(
    conn: &mut SqliteConnection,
    ids: Option<&[i64]>,
    c: &Consulta,
    palabras: &Palabras,
    pedido: Option<&str>,
) -> Resultado<Vec<Fila>> {
    let Some(pedido) = pedido else {
        return Ok(Vec::new());
    };
    let filas: Vec<(i64, DateTime<Utc>, String)> = sqlx::query_as(
        "select f.id, f.fecha, fragmentos_texto.contenido
         from fragmentos_texto join fragmentos f on f.id = fragmentos_texto.rowid
         where fragmentos_texto match ?1
           and (?2 is null or f.grupo_id in (select value from json_each(?2)))
           and (?3 is null or f.fecha >= ?3)
         order by bm25(fragmentos_texto), f.id desc
         limit ?4",
    )
    .bind(pedido)
    .bind(lista(ids))
    .bind(c.desde)
    .bind(i64::from(REVISADOS))
    .fetch_all(conn)
    .await?;
    Ok(filas
        .into_iter()
        .filter(|(_, _, contenido)| palabras.firme(&conjunto(contenido)))
        .take(usize::from(CANDIDATOS))
        .map(|(id, fecha, _)| Fila {
            id,
            estado: EstadoNota::Vigente,
            reemplazada_por: None,
            fecha,
            firme: true,
            suelta: false,
        })
        .collect())
}

fn fragmentos_por_significado(
    indice: &Indice,
    ids: Option<&[i64]>,
    c: &Consulta,
    huellas: &[Vec<f32>],
    piso: f64,
) -> Vec<Fila> {
    let desde = c.desde.map(|d| d.timestamp());
    let entra = |d: &Dato| {
        ids.is_none_or(|ids| d.grupo.is_some_and(|g| ids.contains(&g))) && desde.is_none_or(|desde| d.fecha >= desde)
    };
    cercanas(indice, Huellas::Fragmentos, huellas, &entra)
        .into_iter()
        .filter(|(_, parecido)| *parecido >= piso)
        .map(|(dato, _)| Fila {
            id: dato.id,
            estado: EstadoNota::Vigente,
            reemplazada_por: None,
            fecha: DateTime::from_timestamp(dato.fecha, 0).unwrap_or_default(),
            firme: true,
            suelta: false,
        })
        .collect()
}

/// Suma a los candidatos lo que trajo un paso del alcance, mezclando las dos
/// búsquedas. Lo que ya había aparecido en un paso anterior no se repite.
/// Fuera del grupo de la conversación no entra lo que solo comparte una
/// palabra suelta con la consulta: ahí no sirve ni para completar, porque en
/// toda la base siempre hay alguna nota así.
fn sumar(
    candidatos: &mut Vec<Candidato>,
    vistos: &mut HashSet<(Capa, i64)>,
    paso: Paso,
    palabras: Vec<Fila>,
    significado: Vec<Fila>,
    ahora: DateTime<Utc>,
) {
    let Paso { capa, etapa, del_grupo } = paso;
    let por_palabras: Vec<i64> = palabras.iter().map(|f| f.id).collect();
    let por_significado: Vec<i64> = significado.iter().map(|f| f.id).collect();
    let puntos = fusionar(&[&por_palabras, &por_significado]);
    let firmes: HashSet<i64> = palabras.iter().chain(&significado).filter(|f| f.firme).map(|f| f.id).collect();
    for mut fila in palabras.into_iter().chain(significado) {
        let firme = firmes.contains(&fila.id);
        if !firme && fila.suelta && !del_grupo {
            continue;
        }
        if !vistos.insert((capa, fila.id)) {
            continue;
        }
        fila.firme = firme;
        let puntaje = puntos.get(&fila.id).copied().unwrap_or_default() + bono_reciente(fila.fecha, ahora);
        candidatos.push(Candidato { capa, fila, puntaje, etapa, antes: Vec::new() });
    }
}

/// Cambia cada nota reemplazada por la que vale hoy (siguiendo la cadena de
/// reemplazos), que la recuerda como «antes se usaba».
async fn seguir_reemplazos(conn: &mut SqliteConnection, candidatos: &mut Vec<Candidato>) -> Resultado<()> {
    let es_vieja = |c: &Candidato| c.capa == Capa::Nota && c.fila.estado == EstadoNota::Reemplazada;
    let viejas: Vec<Candidato> = candidatos.iter().filter(|c| es_vieja(c)).cloned().collect();
    if viejas.is_empty() {
        return Ok(());
    }
    candidatos.retain(|c| !es_vieja(c));

    let mut destinos: Vec<Option<i64>> = viejas.iter().map(|v| v.fila.reemplazada_por).collect();
    let mut finales: HashMap<i64, Fila> = HashMap::new();
    for _ in 0..SALTOS_MAXIMOS {
        let pendientes: Vec<i64> = destinos.iter().flatten().copied().filter(|id| !finales.contains_key(id)).collect();
        if pendientes.is_empty() {
            break;
        }
        let filas: Vec<(i64, String, Option<i64>, DateTime<Utc>)> = sqlx::query_as(
            "select id, estado, reemplazada_por, creada from notas where id in (select value from json_each(?1))",
        )
        .bind(base::a_json(&pendientes))
        .fetch_all(&mut *conn)
        .await?;
        let leidas: HashMap<i64, Fila> = filas
            .into_iter()
            .map(|(id, estado_de_nota, reemplazada_por, creada)| {
                let fila = Fila {
                    id,
                    estado: estado(&estado_de_nota),
                    reemplazada_por,
                    fecha: creada,
                    firme: false,
                    suelta: false,
                };
                (id, fila)
            })
            .collect();
        for destino in &mut destinos {
            let Some(id) = *destino else { continue };
            match leidas.get(&id) {
                Some(f) if f.estado == EstadoNota::Reemplazada => *destino = f.reemplazada_por,
                Some(f) => {
                    finales.insert(id, f.clone());
                }
                None if finales.contains_key(&id) => {}
                None => *destino = None,
            }
        }
    }

    for (vieja, destino) in viejas.into_iter().zip(destinos) {
        let Some(actual) = destino.and_then(|id| finales.get(&id)) else { continue };
        if actual.estado != EstadoNota::Vigente {
            continue;
        }
        if let Some(c) = candidatos.iter_mut().find(|c| c.capa == Capa::Nota && c.fila.id == actual.id) {
            c.antes.push(vieja.fila.id);
            c.puntaje = c.puntaje.max(vieja.puntaje);
            c.fila.firme |= vieja.fila.firme;
        } else {
            candidatos.push(Candidato {
                capa: Capa::Nota,
                fila: Fila { firme: vieja.fila.firme, ..actual.clone() },
                puntaje: vieja.puntaje,
                etapa: vieja.etapa,
                antes: vec![vieja.fila.id],
            });
        }
    }
    Ok(())
}

/// Ordena y recorta: notas vigentes, pedazos de conversación, reemplazadas;
/// dentro de cada clase, primero lo firme. Lo que coincide solo en parte
/// entra para completar cuando lo firme no llega a [`MINIMO`]. Si sobran
/// notas, igual quedan unos lugares para los pedazos.
fn elegir(mut candidatos: Vec<Candidato>, limite: usize) -> Vec<Candidato> {
    candidatos.sort_by(|a, b| {
        (a.orden(), !a.fila.firme, a.etapa)
            .cmp(&(b.orden(), !b.fila.firme, b.etapa))
            .then(b.puntaje.total_cmp(&a.puntaje))
            .then(b.fila.id.cmp(&a.fila.id))
    });
    let firmes = candidatos.iter().filter(|c| c.fila.firme).count();
    let mut parciales = MINIMO.saturating_sub(firmes);
    candidatos.retain(|c| {
        if c.fila.firme {
            return true;
        }
        let entra = parciales > 0;
        parciales = parciales.saturating_sub(1);
        entra
    });
    let cuantos = |orden: u8| candidatos.iter().filter(|c| c.orden() == orden).count();
    let (vigentes, fragmentos, viejas) = (cuantos(0), cuantos(1), cuantos(2));
    let reserva = RESERVA_FRAGMENTOS.min(fragmentos).min(limite / 2);
    let de_fragmentos = fragmentos.min(limite - vigentes.min(limite - reserva));
    let de_vigentes = vigentes.min(limite - de_fragmentos);
    let de_viejas = viejas.min(limite - de_vigentes - de_fragmentos);
    let mut quedan = [de_vigentes, de_fragmentos, de_viejas];
    candidatos.retain(|c| {
        let lugar = &mut quedan[usize::from(c.orden())];
        if *lugar == 0 {
            return false;
        }
        *lugar -= 1;
        true
    });
    candidatos
}

struct DetalleNota {
    tipo: TipoNota,
    estado: EstadoNota,
    reemplazada_por: Option<i64>,
    titulo: String,
    origenes: Vec<String>,
    grupo_id: i64,
    grupo: String,
    extracto: String,
}

type FilaDeNota = (i64, String, String, Option<i64>, String, String, String, i64, String, String);

async fn detalle_notas(
    conn: &mut SqliteConnection,
    ids: &[i64],
    palabras: &Palabras,
) -> Resultado<HashMap<i64, DetalleNota>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let filas: Vec<FilaDeNota> = sqlx::query_as(
        "select n.id, n.tipo, n.estado, n.reemplazada_por, n.titulo, n.cuerpo, n.origenes, n.grupo_id,
                g.empresa, g.proyecto
         from notas n join grupos g on g.id = n.grupo_id
         where n.id in (select value from json_each(?1))",
    )
    .bind(base::a_json(ids))
    .fetch_all(conn)
    .await?;
    Ok(filas
        .into_iter()
        .map(|(id, tipo, estado_de_nota, reemplazada_por, titulo, cuerpo, origenes, grupo_id, empresa, proyecto)| {
            let detalle = DetalleNota {
                tipo: tipo.parse().unwrap_or(TipoNota::Dato),
                estado: estado(&estado_de_nota),
                reemplazada_por,
                titulo,
                origenes: base::lista(&origenes),
                grupo_id,
                grupo: grupos::nombre(&empresa, Some(proyecto.as_str()).filter(|p| !p.is_empty())),
                extracto: if cuerpo.chars().count() <= NOTA_ENTERA {
                    cuerpo
                } else {
                    extracto(&cuerpo, palabras, PALABRAS_DE_NOTA)
                },
            };
            (id, detalle)
        })
        .collect())
}

struct DetalleFragmento {
    conversacion: String,
    titulo: Option<String>,
    grupo_id: Option<i64>,
    grupo: String,
    extracto: String,
}

type FilaDeFragmento = (i64, String, Option<String>, Option<i64>, Option<String>, Option<String>, String);

async fn detalle_fragmentos(
    conn: &mut SqliteConnection,
    ids: &[i64],
    palabras: &Palabras,
) -> Resultado<HashMap<i64, DetalleFragmento>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let filas: Vec<FilaDeFragmento> = sqlx::query_as(
        "select f.id, f.conversacion, c.titulo, f.grupo_id, g.empresa, g.proyecto, f.contenido
         from fragmentos f
           join conversaciones c on c.id = f.conversacion
           left join grupos g on g.id = f.grupo_id
         where f.id in (select value from json_each(?1))",
    )
    .bind(base::a_json(ids))
    .fetch_all(conn)
    .await?;
    Ok(filas
        .into_iter()
        .map(|(id, conversacion, titulo, grupo_id, empresa, proyecto, contenido)| {
            let detalle = DetalleFragmento {
                conversacion,
                titulo,
                grupo_id,
                grupo: empresa.as_deref().map_or_else(
                    || "sin grupo".to_owned(),
                    |e| grupos::nombre(e, proyecto.as_deref().filter(|p| !p.is_empty())),
                ),
                extracto: extracto(&contenido, palabras, PALABRAS_DE_FRAGMENTO),
            };
            (id, detalle)
        })
        .collect())
}

/// Completa los candidatos elegidos con lo que se le muestra a Claude.
async fn detallar(
    conn: &mut SqliteConnection,
    elegidos: Vec<Candidato>,
    palabras: &Palabras,
) -> Resultado<Vec<Hallazgo>> {
    let de = |capa: Capa| -> Vec<i64> {
        let mut ids: Vec<i64> = elegidos.iter().filter(|c| c.capa == capa).map(|c| c.fila.id).collect();
        if capa == Capa::Nota {
            ids.extend(elegidos.iter().flat_map(|c| c.antes.iter().copied()));
        }
        ids
    };
    let notas = detalle_notas(conn, &de(Capa::Nota), palabras).await?;
    let fragmentos = detalle_fragmentos(conn, &de(Capa::Fragmento), palabras).await?;
    let mut hallazgos = Vec::with_capacity(elegidos.len());
    for c in elegidos {
        let hallazgo = match c.capa {
            Capa::Nota => {
                let Some(d) = notas.get(&c.fila.id) else { continue };
                Hallazgo {
                    id: c.fila.id,
                    clase: Clase::Nota {
                        tipo: d.tipo,
                        estado: d.estado,
                        reemplazada_por: d.reemplazada_por,
                        origenes: d.origenes.clone(),
                    },
                    titulo: d.titulo.clone(),
                    extracto: d.extracto.clone(),
                    fecha: c.fila.fecha,
                    grupo_id: Some(d.grupo_id),
                    grupo: d.grupo.clone(),
                    puntaje: c.puntaje,
                    etapa: c.etapa,
                    antes: c.antes.iter().filter_map(|id| Some((*id, notas.get(id)?.titulo.clone()))).collect(),
                    parcial: !c.fila.firme,
                }
            }
            Capa::Fragmento => {
                let Some(d) = fragmentos.get(&c.fila.id) else { continue };
                Hallazgo {
                    id: c.fila.id,
                    clase: Clase::Fragmento { conversacion: d.conversacion.clone(), titulo: d.titulo.clone() },
                    titulo: d.titulo.clone().unwrap_or_else(|| "Conversación sin título".to_owned()),
                    extracto: d.extracto.clone(),
                    fecha: c.fila.fecha,
                    grupo_id: d.grupo_id,
                    grupo: d.grupo.clone(),
                    puntaje: c.puntaje,
                    etapa: c.etapa,
                    antes: Vec::new(),
                    parcial: !c.fila.firme,
                }
            }
        };
        hallazgos.push(hallazgo);
    }
    Ok(hallazgos)
}

/// Busca `c` empezando por `grupo`. `huellas` son las de la consulta: la de
/// lo que se escribió y, si tiene palabras con equivalentes, la de esa otra
/// forma de decirlo. Sin ninguna (las huellas no andaban) la búsqueda es solo
/// por palabras.
pub async fn buscar(e: &Estado, grupo: Option<&Grupo>, c: &Consulta, huellas: &[Vec<f32>]) -> Resultado<Busqueda> {
    let palabras = Palabras::de(&c.texto);
    // Una sola lectura: todas las consultas ven la base del mismo momento.
    let mut tx = e.pool.begin().await?;
    let etapas: Vec<(Option<Vec<i64>>, String)> = match grupo {
        Some(g) => {
            let de_la_empresa: Vec<i64> = sqlx::query_scalar("select id from grupos where empresa = ?1")
                .bind(&g.empresa)
                .fetch_all(&mut *tx)
                .await?;
            vec![(Some(vec![g.id]), g.nombre()), (Some(de_la_empresa), g.empresa.clone()), (None, "todo".to_owned())]
        }
        None => vec![(None, "todo".to_owned())],
    };
    let con_fragmentos = c.conversaciones && c.tipo.is_none();
    let pedido = if con_fragmentos { pedido_de_fragmentos(&mut tx, &palabras).await? } else { None };
    let ahora = Utc::now();
    let mut vistos: HashSet<(Capa, i64)> = HashSet::new();
    let mut candidatos: Vec<Candidato> = Vec::new();
    let mut recorrido = Vec::new();
    for (etapa, (ids, nombre)) in etapas.iter().enumerate() {
        recorrido.push(nombre.clone());
        let ids = ids.as_deref();
        // Solo el primer paso, y si se sabe de qué grupo es la conversación,
        // busca dentro de su grupo: los demás traen notas de otros.
        let del_grupo = etapa == 0 && grupo.is_some();
        let por_palabras = notas_por_palabras(&mut tx, ids, c, &palabras).await?;
        let por_significado = notas_por_significado(&mut tx, &e.indice, ids, c, huellas, e.umbrales.piso_notas).await?;
        let paso = Paso { capa: Capa::Nota, etapa, del_grupo };
        sumar(&mut candidatos, &mut vistos, paso, por_palabras, por_significado, ahora);
        if con_fragmentos {
            let por_palabras = fragmentos_por_palabras(&mut tx, ids, c, &palabras, pedido.as_deref()).await?;
            let por_significado = fragmentos_por_significado(&e.indice, ids, c, huellas, e.umbrales.piso_fragmentos);
            let paso = Paso { capa: Capa::Fragmento, etapa, del_grupo };
            sumar(&mut candidatos, &mut vistos, paso, por_palabras, por_significado, ahora);
        }
        if candidatos.iter().filter(|c| c.fila.firme).count() >= MINIMO {
            break;
        }
    }
    if !c.reemplazadas {
        seguir_reemplazos(&mut tx, &mut candidatos).await?;
    }
    let elegidos = elegir(candidatos, limite(c));
    let hallazgos = detallar(&mut tx, elegidos, &palabras).await?;
    tx.commit().await?;
    Ok(Busqueda { hallazgos, recorrido })
}

fn fecha(f: DateTime<Utc>) -> String {
    f.with_timezone(&Local).format("%Y-%m-%d").to_string()
}

/// Lo que se le muestra a Claude: una línea por resultado con id, tipo,
/// estado, fecha y de dónde vino, y debajo el extracto.
#[must_use]
pub fn texto(b: &Busqueda, consulta: &str, grupo: Option<&Grupo>) -> String {
    let donde = b.recorrido.first().cloned().unwrap_or_else(|| "todo".to_owned());
    let ampliado = if b.recorrido.len() > 1 {
        format!(" (había pocos: amplié a {})", b.recorrido[1..].join(" y a "))
    } else {
        String::new()
    };
    if b.hallazgos.is_empty() {
        return format!(
            "Nada para «{consulta}» en {donde}{ampliado}. Si debería haber algo, probá con otras palabras."
        );
    }
    let mut salida = format!(
        "{} {} para «{consulta}» en {donde}{ampliado}:\n",
        b.hallazgos.len(),
        if b.hallazgos.len() == 1 { "resultado" } else { "resultados" }
    );
    for (i, h) in b.hallazgos.iter().enumerate() {
        let otro_grupo = grupo.is_none_or(|g| h.grupo_id != Some(g.id));
        let de = if otro_grupo { format!(" · de {}", h.grupo) } else { String::new() };
        let cabeza = match &h.clase {
            Clase::Nota { tipo, estado, reemplazada_por, origenes } => {
                let estado = match (estado, reemplazada_por) {
                    (EstadoNota::Reemplazada, Some(nueva)) => format!("reemplazada por la nota {nueva}"),
                    (e, _) => e.to_string(),
                };
                let origen = origenes.first().map(|o| format!(" · origen {o}")).unwrap_or_default();
                let parcial = if h.parcial { " · coincide solo en parte" } else { "" };
                format!("[nota {}] {tipo} · {estado} · {}{de}{origen}{parcial} — {}", h.id, fecha(h.fecha), h.titulo)
            }
            Clase::Fragmento { conversacion, .. } => {
                format!("[conversación {conversacion}] {}{de} — «{}»", fecha(h.fecha), h.titulo)
            }
        };
        let _ = writeln!(salida, "{}. {cabeza}", i + 1);
        let _ = writeln!(salida, "   {}", h.extracto.split_whitespace().collect::<Vec<_>>().join(" "));
        for (id, titulo) in &h.antes {
            let _ = writeln!(salida, "   Antes se usaba: [nota {id}] {titulo} (reemplazada)");
        }
    }
    salida.trim_end().to_owned()
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn candidato(capa: Capa, id: i64, estado: EstadoNota, puntaje: f64, etapa: usize) -> Candidato {
        let fila = Fila { id, estado, reemplazada_por: None, fecha: Utc::now(), firme: true, suelta: false };
        Candidato { capa, fila, puntaje, etapa, antes: Vec::new() }
    }

    #[test]
    fn una_palabra_suelta_solo_completa_dentro_del_grupo() {
        let fila = |id: i64, firme: bool, suelta: bool| {
            let mut c = candidato(Capa::Nota, id, EstadoNota::Vigente, 0.0, 0);
            c.fila.firme = firme;
            c.fila.suelta = suelta;
            c.fila
        };
        // La 1 tiene todas las palabras; la 2, una sola; la 3, una parte que
        // alcanza para completar; la 4, una sola, pero además llegó por significado.
        let por_palabras =
            || vec![fila(1, true, false), fila(2, false, true), fila(3, false, false), fila(4, false, true)];
        let por_significado = || vec![fila(4, true, false)];
        let sumadas = |del_grupo: bool| {
            let (mut candidatos, mut vistos) = (Vec::new(), HashSet::new());
            let ahora = Utc::now();
            let paso = Paso { capa: Capa::Nota, etapa: 1, del_grupo };
            sumar(&mut candidatos, &mut vistos, paso, por_palabras(), por_significado(), ahora);
            // Otro paso del alcance trae lo mismo: no repite nada ni deja entrar lo que quedó afuera.
            let otro = Paso { capa: Capa::Nota, etapa: 2, del_grupo: false };
            sumar(&mut candidatos, &mut vistos, otro, por_palabras(), por_significado(), ahora);
            candidatos.iter().map(|c| (c.fila.id, c.fila.firme, c.etapa)).collect::<Vec<_>>()
        };
        assert_eq!(sumadas(true), vec![(1, true, 1), (2, false, 1), (3, false, 1), (4, true, 1)]);
        assert_eq!(sumadas(false), vec![(1, true, 1), (3, false, 1), (4, true, 1)]);
    }

    #[test]
    fn lo_que_aparece_en_las_dos_busquedas_queda_primero() {
        let puntos = fusionar(&[&[1, 2, 3], &[3, 4]]);
        let mut orden: Vec<i64> = puntos.keys().copied().collect();
        orden.sort_by(|a, b| puntos[b].total_cmp(&puntos[a]).then(a.cmp(b)));
        // El 2 y el 4 salieron segundos en una sola búsqueda cada uno: empatan.
        assert_eq!(orden, vec![3, 1, 2, 4]);
        assert!((puntos[&2] - puntos[&4]).abs() < 1e-12);
        assert!((puntos[&1] - 1.0 / 61.0).abs() < 1e-12);
        assert!((puntos[&3] - (1.0 / 63.0 + 1.0 / 61.0)).abs() < 1e-12);
    }

    #[test]
    fn de_varias_huellas_cuenta_el_mejor_parecido() {
        let indice = Indice::default();
        for (id, huella) in [(1, [1.0, 0.0, 0.0]), (2, [0.0, 1.0, 0.0]), (3, [0.0, 0.0, 1.0])] {
            assert!(indice.poner(Huellas::Notas, Dato { id, grupo: None, fecha: 0 }, &huella));
        }
        // La consulta como se escribió queda cerca de la nota 1; dicha con
        // sus equivalentes, todavía más cerca de la 2.
        let huellas = vec![vec![1.0, 0.5, 0.0], vec![0.1, 1.0, 0.0]];
        let halladas = cercanas(&indice, Huellas::Notas, &huellas, &|_| true);
        assert_eq!(halladas.iter().map(|(d, _)| d.id).collect::<Vec<_>>(), vec![2, 1, 3]);
        assert!((halladas[0].1 - 1.0 / 1.01_f64.sqrt()).abs() < 1e-6, "{halladas:?}");
        assert!((halladas[1].1 - 1.0 / 1.25_f64.sqrt()).abs() < 1e-6, "{halladas:?}");
        assert!(halladas[2].1.abs() < 1e-6, "{halladas:?}");
        // Con una sola huella es lo mismo que preguntarle al índice.
        assert_eq!(
            cercanas(&indice, Huellas::Notas, &huellas[..1], &|_| true),
            indice.cercanas(Huellas::Notas, &huellas[0], usize::from(CANDIDATOS), &|_| true)
        );
        assert_eq!(cercanas(&indice, Huellas::Notas, &[], &|_| true), Vec::new());
    }

    #[test]
    fn lo_reciente_suma_un_poco() {
        let ahora = Utc::now();
        let hoy = bono_reciente(ahora, ahora);
        let hace_un_mes = bono_reciente(ahora - chrono::Duration::days(30), ahora);
        assert!((hoy - BONO_RECIENTE).abs() < 1e-12);
        assert!((hace_un_mes - BONO_RECIENTE / 2.0).abs() < 1e-12);
        // No alcanza para pasar a algo que encontraron las dos búsquedas.
        assert!(1.0 / 61.0 + hoy < 2.0 / 62.0);
    }

    #[test]
    fn lo_parcial_solo_completa_cuando_falta_lo_firme() {
        let parcial = |id: i64, puntaje: f64| {
            let mut c = candidato(Capa::Nota, id, EstadoNota::Vigente, puntaje, 0);
            c.fila.firme = false;
            c
        };
        // Una firme y cuatro parciales: entran dos parciales, las mejores, después de la firme.
        let mut candidatos = vec![parcial(1, 0.9), parcial(2, 0.8), parcial(3, 0.7), parcial(4, 0.6)];
        candidatos.push(candidato(Capa::Nota, 9, EstadoNota::Vigente, 0.1, 2));
        assert_eq!(ids(&elegir(candidatos.clone(), 8)), vec![(Capa::Nota, 9), (Capa::Nota, 1), (Capa::Nota, 2)]);
        // Con tres firmes no entra ninguna parcial.
        candidatos.push(candidato(Capa::Nota, 8, EstadoNota::Vigente, 0.1, 0));
        candidatos.push(candidato(Capa::Fragmento, 7, EstadoNota::Vigente, 0.1, 0));
        assert_eq!(ids(&elegir(candidatos, 8)), vec![(Capa::Nota, 8), (Capa::Nota, 9), (Capa::Fragmento, 7)]);
    }

    fn ids(elegidos: &[Candidato]) -> Vec<(Capa, i64)> {
        elegidos.iter().map(|c| (c.capa, c.fila.id)).collect()
    }

    #[test]
    fn ordena_vigentes_pedazos_y_reemplazadas() {
        let candidatos = vec![
            candidato(Capa::Nota, 1, EstadoNota::Reemplazada, 0.9, 0),
            candidato(Capa::Fragmento, 7, EstadoNota::Vigente, 0.8, 0),
            candidato(Capa::Nota, 2, EstadoNota::Vigente, 0.1, 1),
            candidato(Capa::Nota, 3, EstadoNota::Vigente, 0.2, 0),
            candidato(Capa::Nota, 4, EstadoNota::Vigente, 0.3, 0),
        ];
        // Vigentes del grupo por puntaje, después las de afuera, los pedazos y las viejas.
        assert_eq!(
            ids(&elegir(candidatos, 8)),
            vec![(Capa::Nota, 4), (Capa::Nota, 3), (Capa::Nota, 2), (Capa::Fragmento, 7), (Capa::Nota, 1)]
        );
    }

    #[test]
    fn guarda_lugar_para_los_pedazos_de_conversacion() {
        let mut candidatos: Vec<Candidato> = (1..=10)
            .map(|i| candidato(Capa::Nota, i, EstadoNota::Vigente, f64::from(i32::try_from(i).unwrap()), 0))
            .collect();
        candidatos.extend((1..=5).map(|i| candidato(Capa::Fragmento, i, EstadoNota::Vigente, 1.0, 0)));
        let elegidos = elegir(candidatos.clone(), 8);
        assert_eq!(elegidos.iter().filter(|c| c.capa == Capa::Nota).count(), 5);
        assert_eq!(elegidos.iter().filter(|c| c.capa == Capa::Fragmento).count(), 3);
        // Con pocas notas, los pedazos ocupan el resto.
        let pocas: Vec<Candidato> =
            candidatos.into_iter().filter(|c| c.capa == Capa::Fragmento || c.fila.id <= 2).collect();
        let elegidos = elegir(pocas, 8);
        assert_eq!(elegidos.iter().filter(|c| c.capa == Capa::Nota).count(), 2);
        assert_eq!(elegidos.iter().filter(|c| c.capa == Capa::Fragmento).count(), 5);
        // Con un solo lugar gana la nota.
        let uno = vec![
            candidato(Capa::Fragmento, 1, EstadoNota::Vigente, 1.0, 0),
            candidato(Capa::Nota, 1, EstadoNota::Vigente, 0.1, 0),
        ];
        assert_eq!(ids(&elegir(uno, 1)), vec![(Capa::Nota, 1)]);
    }
}
