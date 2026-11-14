//! La batería: preguntas con respuesta esperada, para medir si un cambio
//! del agente mejora la búsqueda o rompe algo que andaba.
//!
//! Tiene dos partes. La inicial viene con el guardián (`bateria/inicial.json`):
//! trae sus propias notas, que se cargan en la copia de la base, y preguntas
//! sobre ellas. La otra crece con el uso: cada reporte aceptado suma su
//! ejemplo a la tabla `bateria`, apuntando a notas reales.

use std::collections::BTreeMap;
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

static NOTA_GUARDADA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"nota (\d+)").expect("patrón válido"));
static RESULTADO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\d+\. \[nota (\d+)\]").expect("patrón válido"));

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
    /// No tiene que aparecer nada.
    pub vacia: bool,
}

/// El resultado de correr toda la batería: qué preguntas pasaron.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Corrida {
    pub resultados: BTreeMap<String, bool>,
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

/// El nombre de la pregunta que sumó una mejora a la tabla.
#[must_use]
pub fn nombre_de_tabla(id: i64) -> String {
    format!("tabla:{id}")
}

/// Carga las notas iniciales en el servicio de prueba y arma sus preguntas.
async fn iniciales(instancia: &Instancia) -> anyhow::Result<Vec<Pregunta>> {
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
    semilla
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
        .collect()
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
    let mut preguntas = iniciales(instancia).await?;
    preguntas.extend(de_la_tabla(&instancia.bd()).await?);
    if preguntas.is_empty() {
        bail!("la batería no tiene preguntas");
    }
    let mut corrida = Corrida::default();
    for pregunta in &preguntas {
        // Solo notas: lo que se esté conversando ese día no tiene que mover el resultado.
        let mut argumentos = json!({ "consulta": pregunta.pregunta, "limite": TOPE, "conversaciones": false });
        if let Some(grupo) = &pregunta.grupo {
            argumentos["grupo"] = json!(grupo);
        }
        let aprobada = match instancia.llamar("memoria_buscar", argumentos).await {
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
/// pregunta que andaba y, si la mejora sumó una pregunta, ahora pasa.
pub fn veredicto(antes: &Corrida, despues: &Corrida, nueva: Option<&str>) -> Result<(), String> {
    let rotas: Vec<&str> = antes
        .resultados
        .iter()
        .filter(|(nombre, pasaba)| **pasaba && despues.resultados.get(*nombre) != Some(&true))
        .map(|(nombre, _)| nombre.split_once(':').map_or(nombre.as_str(), |(_, pregunta)| pregunta))
        .collect();
    if !rotas.is_empty() {
        let lista = rotas.iter().take(3).map(|r| format!("«{r}»")).collect::<Vec<_>>().join(", ");
        return Err(format!(
            "El cambio rompía {} de la batería que antes andaban ({lista}).",
            if rotas.len() == 1 { "una pregunta".to_owned() } else { format!("{} preguntas", rotas.len()) }
        ));
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
        Corrida { resultados: resultados.iter().map(|(n, p)| ((*n).to_owned(), *p)).collect() }
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
