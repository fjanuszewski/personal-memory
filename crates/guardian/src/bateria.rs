//! La batería: preguntas con respuesta esperada, para medir si un cambio
//! del agente mejora la búsqueda o rompe algo que andaba.
//!
//! Tiene dos partes. La inicial viene con el guardián (`bateria/inicial.json`):
//! trae sus propias notas, que se cargan en la copia de la base, y preguntas
//! sobre ellas. La otra crece con el uso: cada reporte aceptado suma su
//! ejemplo a la tabla `bateria`, apuntando a notas reales.
//!
//! Las preguntas de control (las que no tienen que traer nada) miden que la
//! búsqueda no invente resultados, y eso solo se puede juzgar con las notas
//! de la batería, que se sabe que no hablan de esos temas. La copia de la
//! base trae además las notas reales, y de esas el guardián no sabe de qué
//! hablan: el día que una nombra el tema de un control, o habla de algo
//! vecino, encontrarla puede estar bien. Por eso un control falla solo si
//! trae una nota de la batería. Lo que trae de la base real no se juzga: se
//! anota, y al comparar un cambio no puede hacer que traiga más.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, bail};
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use sqlx::Connection;

use crate::base;
use crate::instancia::Instancia;

/// Las preguntas iniciales, metidas en el programa al compilarlo: el agente
/// no las puede cambiar.
const INICIAL: &str = include_str!("../../../bateria/inicial.json");
/// Lo esperado tiene que estar entre los primeros resultados.
const TOPE: u32 = 5;
/// A los controles se les pide más: lo que traen de la base real se compara
/// entre una corrida y otra, y cortado en 5 se compararía mal.
const TOPE_DE_CONTROL: usize = 20;

static NOTA_GUARDADA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"nota (\d+)").expect("patrón válido"));
static RESULTADO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\d+\. \[nota (\d+)\]").expect("patrón válido"));
/// Cualquier resultado numerado, sea una nota o no.
static NUMERADO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\d+\. \[").expect("patrón válido"));

#[derive(Deserialize)]
struct Semilla {
    notas: Vec<NotaSemilla>,
    preguntas: Vec<PreguntaSemilla>,
}

#[derive(Deserialize)]
struct NotaSemilla {
    clave: String,
    grupo: String,
    tipo: String,
    titulo: String,
    cuerpo: String,
}

#[derive(Deserialize)]
struct PreguntaSemilla {
    pregunta: String,
    grupo: String,
    esperadas: Vec<String>,
    #[serde(default)]
    vacia: bool,
}

/// Una pregunta lista para hacerle al servicio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pregunta {
    /// Cómo se la reconoce entre una corrida y otra.
    pub nombre: String,
    pub pregunta: String,
    pub grupo: Option<String>,
    /// Las notas que tienen que aparecer.
    pub esperadas: Vec<i64>,
    /// De control: no tiene que aparecer nada.
    pub vacia: bool,
}

/// El resultado de correr toda la batería: qué preguntas pasaron.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Corrida {
    pub resultados: BTreeMap<String, bool>,
    /// Lo que cada control de la batería inicial trajo de la base real, en
    /// el orden en que salió. Solo figuran los que trajeron algo.
    pub ajenas: BTreeMap<String, Vec<i64>>,
}

impl Corrida {
    #[must_use]
    pub fn aprobadas(&self) -> usize {
        self.resultados.values().filter(|pasa| **pasa).count()
    }

    /// Las preguntas que no pasaron.
    #[must_use]
    pub fn falladas(&self) -> Vec<&str> {
        self.resultados.iter().filter(|(_, pasa)| !**pasa).map(|(nombre, _)| nombre.as_str()).collect()
    }

    /// De 0 a 1: la parte de las preguntas que pasó.
    #[must_use]
    pub fn puntaje(&self) -> f32 {
        if self.resultados.is_empty() {
            return 1.0;
        }
        let parte = |n: usize| f32::from(u16::try_from(n).unwrap_or(u16::MAX));
        parte(self.aprobadas()) / parte(self.resultados.len())
    }
}

/// Los ids de las notas que trae una respuesta de `memoria_buscar`, en orden.
#[must_use]
pub fn notas_encontradas(respuesta: &str) -> Vec<i64> {
    RESULTADO.captures_iter(respuesta).filter_map(|c| c[1].parse().ok()).collect()
}

/// Si una respuesta de `memoria_buscar` contesta bien la pregunta.
#[must_use]
pub fn pasa(pregunta: &Pregunta, respuesta: &str) -> bool {
    if pregunta.vacia {
        return respuesta.starts_with("Nada para");
    }
    let encontradas = notas_encontradas(respuesta);
    !pregunta.esperadas.is_empty() && pregunta.esperadas.iter().all(|id| encontradas.contains(id))
}

/// Lo que trajo un control de la batería inicial: si pasa (no trajo ninguna
/// nota de la batería, que se sabe que no hablan de eso) y qué notas trajo
/// de la base real. Esas no se juzgan: el guardián no sabe de qué hablan.
#[must_use]
pub fn control(respuesta: &str, de_la_bateria: &BTreeSet<i64>) -> (bool, Vec<i64>) {
    if respuesta.starts_with("Nada para") {
        return (true, Vec::new());
    }
    let notas = notas_encontradas(respuesta);
    // Una respuesta que no se sabe leer entera no pasa.
    let entera = !notas.is_empty() && notas.len() == NUMERADO.find_iter(respuesta).count();
    let (propias, ajenas): (Vec<i64>, Vec<i64>) = notas.into_iter().partition(|id| de_la_bateria.contains(id));
    (entera && propias.is_empty(), ajenas)
}

/// Una pregunta como se la nombra al usuario, sin de dónde salió.
fn dicha(nombre: &str) -> &str {
    nombre.split_once(':').map_or(nombre, |(_, pregunta)| pregunta)
}

/// El nombre de la pregunta que sumó una mejora a la tabla.
#[must_use]
pub fn nombre_de_tabla(id: i64) -> String {
    format!("tabla:{id}")
}

/// Carga las notas iniciales en el servicio de prueba y arma sus preguntas.
/// Devuelve también qué notas son las de la batería.
async fn iniciales(instancia: &Instancia) -> anyhow::Result<(Vec<Pregunta>, BTreeSet<i64>)> {
    let semilla: Semilla = serde_json::from_str(INICIAL).context("bateria/inicial.json está mal armado")?;
    let mut ids: BTreeMap<String, i64> = BTreeMap::new();
    for nota in &semilla.notas {
        let respuesta = instancia
            .llamar(
                "memoria_guardar",
                json!({ "tipo": nota.tipo, "titulo": nota.titulo, "cuerpo": nota.cuerpo, "grupo": nota.grupo }),
            )
            .await
            .with_context(|| format!("no pude cargar la nota «{}» de la batería", nota.clave))?;
        let id = NOTA_GUARDADA
            .captures(&respuesta)
            .and_then(|c| c[1].parse().ok())
            .with_context(|| format!("no entendí la respuesta al guardar «{}»: {respuesta}", nota.clave))?;
        ids.insert(nota.clave.clone(), id);
    }
    let preguntas = semilla
        .preguntas
        .into_iter()
        .map(|p| {
            let esperadas = p
                .esperadas
                .iter()
                .map(|clave| {
                    ids.get(clave).copied().with_context(|| format!("la batería nombra una nota que no trae: {clave}"))
                })
                .collect::<anyhow::Result<Vec<i64>>>()?;
            Ok(Pregunta {
                nombre: format!("inicial:{}", p.pregunta),
                pregunta: p.pregunta,
                grupo: Some(p.grupo),
                esperadas,
                vacia: p.vacia,
            })
        })
        .collect::<anyhow::Result<Vec<Pregunta>>>()?;
    Ok((preguntas, ids.into_values().collect()))
}

/// Una fila de la tabla `bateria`: id, pregunta, empresa, proyecto y notas
/// esperadas (una lista escrita como texto).
type FilaDeTabla = (i64, String, Option<String>, Option<String>, String);

/// Las preguntas que fueron sumando los reportes, leídas de la copia de la base.
async fn de_la_tabla(bd: &Path) -> anyhow::Result<Vec<Pregunta>> {
    let mut conexion = base::conexion(bd).await?;
    let filas: Result<Vec<FilaDeTabla>, sqlx::Error> = sqlx::query_as(
        "select b.id, b.pregunta, g.empresa, g.proyecto, b.esperadas
         from bateria b left join grupos g on g.id = b.grupo_id order by b.id",
    )
    .fetch_all(&mut conexion)
    .await;
    let _ = conexion.close().await;
    Ok(filas?
        .into_iter()
        .map(|(id, pregunta, empresa, proyecto, esperadas)| {
            let esperadas: Vec<i64> = serde_json::from_str(&esperadas).unwrap_or_default();
            Pregunta {
                nombre: nombre_de_tabla(id),
                pregunta,
                grupo: empresa.map(|e| match proyecto.filter(|p| !p.is_empty()) {
                    Some(p) => format!("{e} › {p}"),
                    None => e,
                }),
                vacia: esperadas.is_empty(),
                esperadas,
            }
        })
        .collect())
}

/// Corre toda la batería contra un servicio de prueba prendido sobre una copia de la base.
pub async fn correr(instancia: &Instancia) -> anyhow::Result<Corrida> {
    let (de_la_semilla, de_la_bateria) = iniciales(instancia).await?;
    let sumadas = de_la_tabla(&instancia.bd()).await?;
    if de_la_semilla.is_empty() && sumadas.is_empty() {
        bail!("la batería no tiene preguntas");
    }
    let mut corrida = Corrida::default();
    let todas = de_la_semilla.iter().map(|p| (p, true)).chain(sumadas.iter().map(|p| (p, false)));
    for (pregunta, inicial) in todas {
        // Los controles de la batería inicial se juzgan contra sus propias
        // notas. Una pregunta que sumó un reporte y no espera nada, no: ahí
        // no puede aparecer ninguna.
        let de_control = inicial && pregunta.vacia;
        let tope = if de_control { json!(TOPE_DE_CONTROL) } else { json!(TOPE) };
        // Solo notas: lo que se esté conversando ese día no tiene que mover el resultado.
        let mut argumentos = json!({ "consulta": pregunta.pregunta, "limite": tope, "conversaciones": false });
        if let Some(grupo) = &pregunta.grupo {
            argumentos["grupo"] = json!(grupo);
        }
        let aprobada = match instancia.llamar("memoria_buscar", argumentos).await {
            Ok(respuesta) if de_control => {
                let (sin_inventar, ajenas) = control(&respuesta, &de_la_bateria);
                if !ajenas.is_empty() {
                    corrida.ajenas.insert(pregunta.nombre.clone(), ajenas);
                }
                sin_inventar
            }
            Ok(respuesta) => pasa(pregunta, &respuesta),
            Err(falla) => {
                tracing::warn!(pregunta = %pregunta.pregunta, falla = format!("{falla:#}"), "una pregunta de la batería dio error");
                false
            }
        };
        corrida.resultados.insert(pregunta.nombre.clone(), aprobada);
    }
    Ok(corrida)
}

/// Decide si el cambio se acepta: el puntaje no baja, no se rompe ninguna
/// pregunta que andaba, ningún control trae de la base real notas que antes
/// no traía y, si la mejora sumó una pregunta, ahora pasa.
pub fn veredicto(antes: &Corrida, despues: &Corrida, nueva: Option<&str>) -> Result<(), String> {
    let rotas: Vec<&str> = antes
        .resultados
        .iter()
        .filter(|(nombre, pasaba)| **pasaba && despues.resultados.get(*nombre) != Some(&true))
        .map(|(nombre, _)| dicha(nombre))
        .collect();
    if !rotas.is_empty() {
        let lista = rotas.iter().take(3).map(|r| format!("«{r}»")).collect::<Vec<_>>().join(", ");
        return Err(format!(
            "El cambio rompía {} de la batería que antes andaban ({lista}).",
            if rotas.len() == 1 { "una pregunta".to_owned() } else { format!("{} preguntas", rotas.len()) }
        ));
    }
    // Si antes ya llenaba el tope no hay cómo comparar: cambia cuáles entran.
    let con_nuevas: Vec<&str> = despues
        .ajenas
        .iter()
        .filter(|(nombre, notas)| {
            let previas = antes.ajenas.get(*nombre).map_or(&[][..], Vec::as_slice);
            previas.len() < TOPE_DE_CONTROL && notas.iter().any(|id| !previas.contains(id))
        })
        .map(|(nombre, _)| dicha(nombre))
        .collect();
    if !con_nuevas.is_empty() {
        let lista = con_nuevas.iter().take(3).map(|c| format!("«{c}»")).collect::<Vec<_>>().join(", ");
        return Err(if con_nuevas.len() == 1 {
            format!("El cambio hacía que una pregunta de control ({lista}) trajera notas que antes no traía.")
        } else {
            format!(
                "El cambio hacía que {} preguntas de control ({lista}) trajeran notas que antes no traían.",
                con_nuevas.len()
            )
        });
    }
    if despues.puntaje() < antes.puntaje() {
        return Err(format!(
            "El cambio bajaba el puntaje de la batería (de {} a {} preguntas bien).",
            antes.aprobadas(),
            despues.aprobadas()
        ));
    }
    if let Some(nueva) = nueva
        && despues.resultados.get(nueva) != Some(&true)
    {
        return Err(
            "El cambio no arreglaba el ejemplo del reporte: la búsqueda sigue sin traer lo esperado.".to_owned()
        );
    }
    Ok(())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn corrida(resultados: &[(&str, bool)]) -> Corrida {
        Corrida { resultados: resultados.iter().map(|(n, p)| ((*n).to_owned(), *p)).collect(), ..Corrida::default() }
    }

    /// La misma corrida, con lo que cada control trajo de la base real.
    fn con_ajenas(corrida: &Corrida, ajenas: &[(&str, &[i64])]) -> Corrida {
        Corrida {
            ajenas: ajenas.iter().map(|(nombre, notas)| ((*nombre).to_owned(), notas.to_vec())).collect(),
            ..corrida.clone()
        }
    }

    fn pregunta(esperadas: &[i64], vacia: bool) -> Pregunta {
        Pregunta { nombre: "x".into(), pregunta: "x".into(), grupo: None, esperadas: esperadas.to_vec(), vacia }
    }

    const RESPUESTA: &str = "3 resultados para «login» en ACME › Portal:\n1. [nota 12] error · vigente · 2026-11-13 — Error de sesión\n   Error: la app pedía login. Ver [nota 99].\n2. [conversación local_a] 2026-11-12 — «Charla»\n   Usuario: el login\n3. [nota 7] decisión · vigente · 2026-11-10 · coincide solo en parte — Usar Cognito\n   Qué: Cognito.\n   Antes se usaba: [nota 3] Login con Auth0 (reemplazada)";

    #[test]
    fn lee_los_resultados_de_una_busqueda() {
        // Solo cuentan los resultados numerados: ni lo que nombra un extracto ni las reemplazadas.
        assert_eq!(notas_encontradas(RESPUESTA), vec![12, 7]);
        let nada = "Nada para «x» en todo. Si debería haber algo, probá con otras palabras.";
        assert_eq!(notas_encontradas(nada), Vec::<i64>::new());
    }

    #[test]
    fn una_pregunta_pasa_si_aparece_todo_lo_esperado() {
        assert!(pasa(&pregunta(&[12], false), RESPUESTA));
        assert!(pasa(&pregunta(&[7, 12], false), RESPUESTA));
        assert!(!pasa(&pregunta(&[12, 3], false), RESPUESTA));
        assert!(!pasa(&pregunta(&[], false), RESPUESTA), "sin nada esperado no hay cómo pasar");
        assert!(pasa(
            &pregunta(&[], true),
            "Nada para «tarta» en Batería › Producto (había pocos: amplié a Batería y a todo)."
        ));
        assert!(!pasa(&pregunta(&[], true), RESPUESTA));
    }

    #[test]
    fn la_semilla_esta_bien_armada() {
        let semilla: Semilla = serde_json::from_str(INICIAL).unwrap();
        let claves: Vec<&str> = semilla.notas.iter().map(|n| n.clave.as_str()).collect();
        assert!(semilla.notas.len() >= 10 && semilla.preguntas.len() >= 20);
        for p in &semilla.preguntas {
            assert_eq!(p.vacia, p.esperadas.is_empty(), "{}", p.pregunta);
            assert!(p.esperadas.iter().all(|e| claves.contains(&e.as_str())), "{}", p.pregunta);
        }
        let mut sin_repetir = claves.clone();
        sin_repetir.sort_unstable();
        sin_repetir.dedup();
        assert_eq!(sin_repetir.len(), claves.len(), "hay claves repetidas");
    }

    #[test]
    fn un_control_falla_solo_si_trae_una_nota_de_la_bateria() {
        let de_la_bateria = BTreeSet::from([101, 102]);
        let nada = "Nada para «tarta» en Batería › Producto (había pocos: amplié a Batería y a todo).";
        assert_eq!(control(nada, &de_la_bateria), (true, Vec::new()));
        // Lo que trae de la base real no lo hace fallar: queda anotado.
        let de_la_base = "2 resultados para «kubernetes» en Batería › Producto (había pocos: amplié a Batería y a todo):\n1. [nota 26] decisión · vigente · 2026-11-16 · de ACME › Portal — Staging local con docker\n   Qué: todo en contenedores.\n2. [nota 7] dato · vigente · 2026-11-10 · de ACME — Otra\n   Ver [nota 101].";
        assert_eq!(control(de_la_base, &de_la_bateria), (true, vec![26, 7]));
        // Una sola nota de la batería alcanza para que falle: esas no hablan de eso.
        let inventada = "2 resultados para «tarta» en Batería › Producto:\n1. [nota 26] decisión · vigente · 2026-11-16 · de ACME › Portal — Staging\n   Qué: docker.\n2. [nota 102] dato · vigente · 2026-11-10 — Facturación de octubre\n   Se facturaron 62 horas.";
        assert_eq!(control(inventada, &de_la_bateria), (false, vec![26]));
        // Una respuesta que no se sabe leer entera no pasa.
        assert_eq!(control(RESPUESTA, &de_la_bateria), (false, vec![12, 7]));
        assert_eq!(control("Una respuesta que no se entiende.", &de_la_bateria), (false, Vec::new()));
    }

    #[test]
    fn un_control_no_puede_traer_de_la_base_notas_que_antes_no_traia() {
        let base = corrida(&[("inicial:login", true), ("inicial:rosal", true), ("inicial:kubernetes", true)]);
        let antes = con_ajenas(&base, &[("inicial:rosal", &[25, 23, 16])]);
        // Trae lo mismo, menos, o ya nada: se acepta.
        assert_eq!(veredicto(&antes, &antes, None), Ok(()));
        assert_eq!(veredicto(&antes, &con_ajenas(&base, &[("inicial:rosal", &[25])]), None), Ok(()));
        assert_eq!(veredicto(&antes, &base, None), Ok(()));
        // Trae una que antes no traía.
        let motivo = veredicto(&antes, &con_ajenas(&base, &[("inicial:rosal", &[25, 40])]), None).unwrap_err();
        assert!(motivo.contains("una pregunta de control («rosal») trajera notas que antes no traía"), "{motivo}");
        // También si antes no traía nada, y se nombran todas las que cambiaron.
        let dos = con_ajenas(&base, &[("inicial:rosal", &[25, 40]), ("inicial:kubernetes", &[26])]);
        let motivo = veredicto(&antes, &dos, None).unwrap_err();
        assert!(motivo.contains("2 preguntas de control («kubernetes», «rosal») trajeran"), "{motivo}");
        // Si antes ya llenaba el tope, no hay cómo comparar.
        let lleno: Vec<i64> = (1..=20).collect();
        let antes = con_ajenas(&base, &[("inicial:rosal", &lleno)]);
        assert_eq!(veredicto(&antes, &con_ajenas(&base, &[("inicial:rosal", &[99])]), None), Ok(()));
    }

    #[test]
    fn el_puntaje_es_la_parte_que_pasa() {
        assert!((corrida(&[("a", true), ("b", false), ("c", true), ("d", true)]).puntaje() - 0.75).abs() < 1e-6);
        assert!((Corrida::default().puntaje() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn acepta_solo_lo_que_no_rompe_nada() {
        let antes = corrida(&[("inicial:login", true), ("inicial:tarta", true), ("tabla:4", false)]);
        // Arregla el ejemplo del reporte sin romper nada.
        let mejor = corrida(&[("inicial:login", true), ("inicial:tarta", true), ("tabla:4", true)]);
        assert_eq!(veredicto(&antes, &mejor, Some("tabla:4")), Ok(()));
        // Igual que antes y sin pregunta nueva: se acepta.
        assert_eq!(veredicto(&antes, &antes, None), Ok(()));
        // Arregla el ejemplo pero rompe otra.
        let rompe = corrida(&[("inicial:login", false), ("inicial:tarta", true), ("tabla:4", true)]);
        let motivo = veredicto(&antes, &rompe, Some("tabla:4")).unwrap_err();
        assert!(motivo.contains("rompía una pregunta") && motivo.contains("«login»"), "{motivo}");
        // No rompe nada pero no arregla lo que se reportó.
        let motivo = veredicto(&antes, &antes, Some("tabla:4")).unwrap_err();
        assert!(motivo.contains("no arreglaba el ejemplo"), "{motivo}");
        // Una pregunta que desaparece cuenta como rota.
        let menos = corrida(&[("inicial:login", true), ("tabla:4", true)]);
        assert!(veredicto(&antes, &menos, None).unwrap_err().contains("rompía"));
    }
}
