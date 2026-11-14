//! El recorrido de una mejora aceptada (punto 4.8 del diseño): sumar su
//! ejemplo a la batería, armar la copia, hacer trabajar al agente, revisar,
//! compilar, probar, medir, instalar y dejar anotado cómo terminó.

use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex;

use crate::agente::Agente;
use crate::bateria::{self, Corrida};
use crate::config::Config;
use crate::revision::{self, Permisos};
use crate::{base, git, instalar, instancia, orden};

/// Cuánto puede tardar cada paso de compilar o probar.
const LIMITE_CARGO: Duration = Duration::from_mins(25);
/// Una mejora que quedó vieja porque otra se instaló antes se rehace una vez.
const INTENTOS: i32 = 2;

pub struct Guardian {
    pub cfg: Config,
    pub pool: SqlitePool,
    pub agente: Arc<dyn Agente>,
    /// Se instala de a una.
    instalacion: Mutex<()>,
}

/// Una mejora aceptada, con lo que hace falta para trabajarla.
#[derive(Clone, Debug)]
pub struct Mejora {
    pub id: i64,
    pub resumen: String,
    pub detalle: String,
    pub ejemplo: Value,
    pub grupo_id: Option<i64>,
    pub grupo: Option<String>,
    pub permisos: Permisos,
    pub intentos: i32,
}

/// Cómo terminó el trabajo sobre una mejora.
#[derive(Clone, Debug, PartialEq)]
pub enum Desenlace {
    Instalada {
        cambios: String,
        antes: String,
        despues: String,
        puntajes: (f32, f32),
        rama: String,
    },
    /// No se instaló nada: todo quedó como estaba.
    Fallida {
        motivo: String,
        puntajes: Option<(f32, f32)>,
        rama: Option<String>,
    },
    /// Otra mejora se instaló mientras tanto: hay que rehacerla sobre lo nuevo.
    Reintentar,
}

/// Por qué no se instaló, con lo que se haya llegado a medir.
struct Falla {
    motivo: String,
    puntajes: Option<(f32, f32)>,
    rama: Option<String>,
}

impl From<String> for Falla {
    fn from(motivo: String) -> Self {
        Self { motivo, puntajes: None, rama: None }
    }
}

impl From<anyhow::Error> for Falla {
    fn from(falla: anyhow::Error) -> Self {
        format!("{falla:#}").into()
    }
}

fn si_o_no(valor: bool) -> &'static str {
    if valor { "sí" } else { "no" }
}

/// Lo que se le pide al agente: el reporte y lo que tiene permitido.
#[must_use]
pub fn pedido(m: &Mejora) -> String {
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

impl Guardian {
    #[must_use]
    pub fn nuevo(cfg: Config, pool: SqlitePool, agente: Arc<dyn Agente>) -> Self {
        Self { cfg, pool, agente, instalacion: Mutex::new(()) }
    }

    /// Lo que había quedado a medias de una corrida anterior del guardián
    /// vuelve a la fila.
    pub async fn recuperar(&self) -> anyhow::Result<u64> {
        let filas = sqlx::query("update mejoras set estado = 'aceptada' where estado = 'en_curso'")
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(filas)
    }

    /// Toma la próxima mejora aceptada, si hay, y la marca en curso.
    pub async fn tomar(&self) -> anyhow::Result<Option<Mejora>> {
        // Con el turno de escritura tomado desde el principio, dos tareas
        // nunca se llevan la misma mejora.
        let mut tx = self.pool.begin_with("begin immediate").await?;
        let proxima: Option<i64> =
            sqlx::query_scalar("select id from mejoras where estado = 'aceptada' order by creada, id limit 1")
                .fetch_optional(&mut *tx)
                .await?;
        let Some(id) = proxima else {
            return Ok(None);
        };
        sqlx::query("update mejoras set estado = 'en_curso', empezada = ?2, intentos = intentos + 1 where id = ?1")
            .bind(id)
            .bind(chrono::Utc::now())
            .execute(&mut *tx)
            .await?;
        let f = sqlx::query(
            "select m.id, m.resumen, m.detalle, m.ejemplo, m.grupo_id, m.toca_base, m.borra_datos,
                    m.suma_librerias, m.intentos, g.empresa, g.proyecto
             from mejoras m left join grupos g on g.id = m.grupo_id
             where m.id = ?1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let empresa: Option<String> = f.get("empresa");
        let proyecto: Option<String> = f.get("proyecto");
        let ejemplo: String = f.get("ejemplo");
        Ok(Some(Mejora {
            id: f.get("id"),
            resumen: f.get("resumen"),
            detalle: f.get("detalle"),
            ejemplo: serde_json::from_str(&ejemplo).unwrap_or(Value::Null),
            grupo_id: f.get("grupo_id"),
            grupo: empresa.map(|e| match proyecto.filter(|p| !p.is_empty()) {
                Some(p) => format!("{e} › {p}"),
                None => e,
            }),
            permisos: Permisos {
                toca_base: f.get("toca_base"),
                borra_datos: f.get("borra_datos"),
                suma_librerias: f.get("suma_librerias"),
            },
            intentos: f.get("intentos"),
        }))
    }

    /// Suma el ejemplo del reporte a la batería, antes de que el agente
    /// empiece. Devuelve el nombre de esa pregunta, si el reporte traía qué
    /// se buscó y qué notas tenían que aparecer.
    async fn sumar_a_la_bateria(&self, m: &Mejora) -> anyhow::Result<Option<String>> {
        let buscado = m.ejemplo["buscado"].as_str().map(str::trim).filter(|b| !b.is_empty());
        let esperadas: Vec<i64> =
            m.ejemplo["notas_esperadas"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
        let (Some(buscado), false) = (buscado, esperadas.is_empty()) else {
            return Ok(None);
        };
        let esperadas = serde_json::to_string(&esperadas)?;
        let mut tx = self.pool.begin_with("begin immediate").await?;
        // La misma pregunta en el mismo grupo no entra dos veces: se actualiza.
        let anterior: Option<i64> = sqlx::query_scalar("select id from bateria where pregunta = ?1 and grupo_id is ?2")
            .bind(buscado)
            .bind(m.grupo_id)
            .fetch_optional(&mut *tx)
            .await?;
        let id = if let Some(id) = anterior {
            sqlx::query("update bateria set esperadas = ?2 where id = ?1")
                .bind(id)
                .bind(&esperadas)
                .execute(&mut *tx)
                .await?;
            id
        } else {
            sqlx::query_scalar(
                "insert into bateria (pregunta, grupo_id, esperadas, desde_mejora, creada)
                 values (?1, ?2, ?3, ?4, ?5)
                 returning id",
            )
            .bind(buscado)
            .bind(m.grupo_id)
            .bind(&esperadas)
            .bind(m.id)
            .bind(chrono::Utc::now())
            .fetch_one(&mut *tx)
            .await?
        };
        tx.commit().await?;
        Ok(Some(bateria::nombre_de_tabla(id)))
    }

    fn entorno_de_cargo(&self, dir: &Path) -> Vec<(&'static str, String)> {
        let mut entorno = self.cfg.entorno();
        entorno.push(("CARGO_TARGET_DIR", git::compilados_de(dir).display().to_string()));
        entorno
    }

    /// Prueba las migraciones de la copia de trabajo sobre una copia de la
    /// base real: así una migración nueva se prueba con los datos de verdad.
    async fn probar_migraciones(&self, m: &Mejora, dir: &Path) -> anyhow::Result<()> {
        let cfg = &self.cfg;
        let copia = cfg.trabajo().join(format!("mejora-{}-migracion.db", m.id));
        base::volcar(cfg, &cfg.bd, &copia).await?;
        let resultado = aplicar_migraciones(dir, &copia).await;
        let _ = base::borrar(cfg, &copia);
        resultado
    }

    /// Compila y prueba lo que dejó el agente. `Err` trae el motivo.
    async fn compilar_y_probar(&self, m: &Mejora, dir: &Path, con_migracion: bool) -> Result<(), String> {
        let entorno = self.entorno_de_cargo(dir);
        if con_migracion {
            self.probar_migraciones(m, dir)
                .await
                .map_err(|e| format!("La migración nueva no se pudo aplicar sobre una copia de la base: {e:#}"))?;
        }
        let cargo = |args: &[&str]| orden::correr(Path::new("cargo"), args, dir, &entorno, LIMITE_CARGO);
        // Las órdenes quedan armadas; cada una se lanza recién cuando le toca.
        let pasos = [
            ("El cambio no compila", cargo(&["build", "--release", "--locked", "-p", "memoria"])),
            ("Con el cambio no pasan las pruebas", cargo(&["test", "-p", "memoria"])),
            (
                "El cambio deja avisos del revisor de código",
                cargo(&["clippy", "-p", "memoria", "--all-targets", "--", "-D", "warnings"]),
            ),
        ];
        for (problema, paso) in pasos {
            let salida = paso.await.map_err(|e| format!("{problema}: {e:#}"))?;
            if !salida.ok {
                tracing::warn!(problema, salida = %salida.cola(), "el cambio del agente no pasó un paso");
                let final_de_salida: String =
                    salida.cola().chars().rev().take(600).collect::<String>().chars().rev().collect();
                return Err(format!("{problema}. Lo último que dijo: {final_de_salida}"));
            }
        }
        Ok(())
    }

    /// Corre la batería con el programa instalado y con el candidato, cada
    /// uno sobre una copia igual de la base.
    async fn medir(&self, m: &Mejora, candidato: &Path) -> anyhow::Result<(Corrida, Corrida)> {
        let cfg = &self.cfg;
        let foto = foto_de_la_base(cfg, m.id);
        base::volcar(cfg, &cfg.bd, &foto).await?;
        let puerto = cfg.puerto_pruebas + u16::try_from(m.id.rem_euclid(50)).unwrap_or(0) * 2;
        let mut corridas = Vec::new();
        for (momento, programa, puerto) in
            [("antes", cfg.programa(), puerto), ("despues", candidato.to_owned(), puerto + 1)]
        {
            let dir = cfg.trabajo().join(format!("mejora-{}-{momento}", m.id));
            let prendida = instancia::prender(cfg, &programa, Some(&foto), puerto, &dir).await;
            let corrida = match prendida {
                Ok(instancia) => {
                    let corrida = match instancia.esperar_huellas().await {
                        Ok(()) => bateria::correr(&instancia).await,
                        Err(falla) => Err(falla),
                    };
                    instancia.apagar().await;
                    corrida
                }
                Err(falla) => {
                    Err(falla.context(format!("el programa de «{momento}» no arrancó sobre una copia de la base")))
                }
            };
            let _ = std::fs::remove_dir_all(&dir);
            corridas.push(corrida?);
        }
        let _ = base::borrar(cfg, &foto);
        let despues = corridas.pop().context("faltó la corrida de después")?;
        let antes = corridas.pop().context("faltó la corrida de antes")?;
        Ok((antes, despues))
    }

    async fn intentar(&self, m: &Mejora) -> Result<Desenlace, Falla> {
        let cfg = &self.cfg;
        let base_git = git::version_instalada(cfg).await?;
        let nueva = self.sumar_a_la_bateria(m).await?;
        let (dir, rama) = git::copia_nueva(cfg, m.id, m.intentos, &base_git).await?;

        tracing::info!(mejora = m.id, intento = m.intentos, base = %base_git, "el agente empieza a trabajar");
        let informe = self.agente.trabajar(&dir, &pedido(m)).await?;
        let cambios = git::cambios(cfg, &dir).await?;
        if cambios.is_empty() {
            return Err(format!("El agente revisó y no cambió nada. {}", informe.resumen).into());
        }
        revision::revisar_rutas(&cambios, m.permisos)?;
        let mut con_migracion = false;
        for cambio in cambios.iter().filter(|c| c.ruta.starts_with("migraciones/")) {
            let contenido = std::fs::read_to_string(dir.join(&cambio.ruta)).unwrap_or_default();
            revision::revisar_migracion(&cambio.ruta, &contenido, m.permisos)?;
            con_migracion = true;
        }
        self.compilar_y_probar(m, &dir, con_migracion).await?;

        let mensaje = format!(
            "Mejora {}: {}\n\n{}\n\nHecha por el agente de mantenimiento de la memoria y revisada por el guardián.",
            m.id, m.resumen, informe.resumen
        );
        let commit = git::guardar(cfg, &dir, &mensaje).await?;
        let candidato = candidato(cfg, m.id);
        std::fs::copy(git::compilados_de(&dir).join("release/memoria"), &candidato)
            .context("no encontré el programa recién compilado")?;

        let (antes, despues) = self.medir(m, &candidato).await?;
        let puntajes = (antes.puntaje(), despues.puntaje());
        tracing::info!(
            mejora = m.id,
            antes = antes.aprobadas(),
            despues = despues.aprobadas(),
            de = despues.resultados.len(),
            fallaban = ?antes.falladas(),
            fallan = ?despues.falladas(),
            "batería corrida"
        );
        if let Err(motivo) = bateria::veredicto(&antes, &despues, nueva.as_deref()) {
            return Err(Falla { motivo, puntajes: Some(puntajes), rama: Some(rama) });
        }

        let _turno = self.instalacion.lock().await;
        if git::version_instalada(cfg).await? != base_git {
            return Ok(Desenlace::Reintentar);
        }
        if let Err(motivo) = instalar::instalar(cfg, m.id, &candidato).await {
            return Err(Falla { motivo, puntajes: Some(puntajes), rama: Some(rama) });
        }
        git::anotar_instalada(cfg, &commit)?;
        if !git::adelantar_principal(cfg, &base_git, &rama).await {
            tracing::info!(mejora = m.id, rama, "la mejora quedó en su rama: la copia del código no se adelantó");
        }
        Ok(Desenlace::Instalada {
            cambios: informe.resumen,
            antes: corto(&base_git),
            despues: corto(&commit),
            puntajes,
            rama,
        })
    }

    /// Trabaja una mejora de punta a punta y limpia lo que haya armado.
    pub async fn trabajar(&self, m: &Mejora) -> Desenlace {
        let desenlace = match self.intentar(m).await {
            Ok(desenlace) => desenlace,
            Err(Falla { motivo, puntajes, rama }) => Desenlace::Fallida { motivo, puntajes, rama },
        };
        let cfg = &self.cfg;
        let copia = cfg.trabajo().join(format!("mejora-{}", m.id));
        // Lo compilado de una mejora instalada queda como punto de partida de la próxima.
        let compilados = git::compilados_de(&copia);
        if matches!(desenlace, Desenlace::Instalada { .. }) && compilados.is_dir() {
            let _ = std::fs::remove_dir_all(git::compilados_base(cfg));
            let _ = std::fs::rename(&compilados, git::compilados_base(cfg));
        }
        let _ = std::fs::remove_dir_all(&compilados);
        git::quitar_copia(cfg, &copia).await;
        let _ = std::fs::remove_file(candidato(cfg, m.id));
        let _ = base::borrar(cfg, &foto_de_la_base(cfg, m.id));
        let _ = base::borrar(cfg, &cfg.trabajo().join(format!("mejora-{}-migracion.db", m.id)));
        for momento in ["antes", "despues"] {
            let _ = std::fs::remove_dir_all(cfg.trabajo().join(format!("mejora-{}-{momento}", m.id)));
        }
        desenlace
    }

    /// Trabaja una mejora y deja anotado cómo terminó. El futuro va en una
    /// caja con su tipo dicho a mano: así el compilador revisa acá, con los
    /// préstamos a la vista, que se puede mandar a otra tarea.
    pub fn atender_una<'a>(&'a self, m: &'a Mejora) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let desenlace = self.trabajar(m).await;
            if let Err(falla) = self.anotar(m, &desenlace).await {
                tracing::error!(mejora = m.id, falla = format!("{falla:#}"), "no pude anotar cómo terminó");
            }
        })
    }

    /// Deja anotado en la base cómo terminó, para que el mod avise en el chat.
    pub async fn anotar(&self, m: &Mejora, desenlace: &Desenlace) -> anyhow::Result<()> {
        let agotada = Desenlace::Fallida {
            motivo: "Otra mejora se instaló antes y esta no se pudo rehacer sobre la versión nueva.".to_owned(),
            puntajes: None,
            rama: None,
        };
        let desenlace = match desenlace {
            Desenlace::Reintentar if m.intentos < INTENTOS => {
                sqlx::query("update mejoras set estado = 'aceptada', empezada = null where id = ?1")
                    .bind(m.id)
                    .execute(&self.pool)
                    .await?;
                tracing::info!(mejora = m.id, "otra mejora se instaló antes: esta se rehace sobre la versión nueva");
                return Ok(());
            }
            Desenlace::Reintentar => &agotada,
            otro => otro,
        };
        let (estado, motivo, cambios, antes, despues, puntajes, rama) = match desenlace {
            Desenlace::Instalada { cambios, antes, despues, puntajes, rama } => (
                "instalada",
                None,
                Some(cambios.as_str()),
                Some(antes.as_str()),
                Some(despues.as_str()),
                Some(*puntajes),
                Some(rama.as_str()),
            ),
            Desenlace::Fallida { motivo, puntajes, rama } => {
                ("fallida", Some(motivo.as_str()), None, None, None, *puntajes, rama.as_deref())
            }
            Desenlace::Reintentar => unreachable!("ya se resolvió arriba"),
        };
        sqlx::query(
            "update mejoras set estado = ?2, motivo = ?3, cambios = ?4, version_antes = ?5,
                    version_despues = ?6, puntaje_antes = ?7, puntaje_despues = ?8, rama = ?9,
                    terminada = ?10, avisada = 0
             where id = ?1",
        )
        .bind(m.id)
        .bind(estado)
        .bind(motivo)
        .bind(cambios)
        .bind(antes)
        .bind(despues)
        .bind(puntajes.map(|p| p.0))
        .bind(puntajes.map(|p| p.1))
        .bind(rama)
        .bind(chrono::Utc::now())
        .execute(&self.pool)
        .await?;
        tracing::info!(mejora = m.id, estado, motivo, "mejora terminada");
        Ok(())
    }
}

fn corto(commit: &str) -> String {
    commit.chars().take(7).collect()
}

fn candidato(cfg: &Config, id: i64) -> PathBuf {
    cfg.trabajo().join(format!("mejora-{id}.candidato"))
}

/// La copia de la base real sobre la que se mide una mejora.
fn foto_de_la_base(cfg: &Config, id: i64) -> PathBuf {
    cfg.trabajo().join(format!("mejora-{id}-foto.db"))
}

/// Aplica a la base `bd` las migraciones de la copia de trabajo.
async fn aplicar_migraciones(dir: &Path, bd: &Path) -> anyhow::Result<()> {
    let migraciones = Migrator::new(dir.join("migraciones")).await.context("no pude leer las migraciones")?;
    // Con un grupo de una sola conexión: aplicar las migraciones sobre una
    // conexión suelta no se puede mandar a otra tarea.
    let opciones = SqliteConnectOptions::new().filename(bd).foreign_keys(true);
    let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opciones).await?;
    let resultado = migraciones.run(&pool).await;
    pool.close().await;
    resultado.context("no pude aplicar las migraciones")
}

#[cfg(test)]
mod pruebas {
    use serde_json::json;

    use super::*;

    #[test]
    fn el_pedido_trae_el_ejemplo_y_los_permisos() {
        let mut m = Mejora {
            id: 7,
            resumen: "La búsqueda de «entregas» no trae el acuerdo.".into(),
            detalle: "Busqué eso y no apareció nada.".into(),
            ejemplo: json!({ "buscado": "cuándo se entrega", "esperado": "la nota del acuerdo", "obtenido": "nada", "notas_esperadas": [4, 9] }),
            grupo_id: Some(2),
            grupo: Some("ACME › Portal".into()),
            permisos: Permisos { toca_base: true, ..Permisos::default() },
            intentos: 1,
        };
        let texto = pedido(&m);
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
        let texto = pedido(&m);
        assert!(
            !texto.contains("Qué se buscó") && !texto.contains("En el grupo") && !texto.contains("Notas que"),
            "{texto}"
        );
    }

    #[test]
    fn nombra_las_bases_de_cada_mejora() {
        assert_eq!(corto("7c00fea1234567890"), "7c00fea");
    }
}
