//! El recorrido de una mejora aceptada (punto 4.8 del diseño), en tres
//! tramos. Primero el guardián la prepara: suma su ejemplo a la batería, arma
//! la copia del código y deja el encargo. Después espera: el agente que hace
//! el cambio corre de fondo dentro de una conversación de Claude, lanzado por
//! el mod. Cuando el agente termina, el guardián revisa lo que tocó, compila,
//! prueba, mide, instala y deja anotado cómo terminó.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde_json::Value;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex;

use crate::bateria::{self, Corrida};
use crate::config::Config;
use crate::encargo::{self, Encargo};
use crate::revision::{self, Permisos};
use crate::{base, git, instalar, instancia, orden};

/// Cuánto puede tardar cada paso de compilar o probar.
const LIMITE_CARGO: Duration = Duration::from_mins(25);
/// Una mejora que quedó vieja porque otra se instaló antes se rehace una vez.
const INTENTOS: i32 = 2;
/// Cuántos agentes se lanzan, como mucho, en cada intento.
const LANZAMIENTOS: i32 = 3;
/// Cuánto puede trabajar un agente en una mejora.
const LIMITE_AGENTE: Duration = Duration::from_mins(45);
/// El mod avisa cada pocos segundos que su agente sigue andando. Si deja de
/// avisar este rato, es que la conversación se cerró.
const SIN_SENAL: Duration = Duration::from_mins(2);

pub struct Guardian {
    pub cfg: Config,
    pub pool: SqlitePool,
    /// Se instala de a una.
    instalacion: Mutex<()>,
    /// Lo último que se supo de cada agente que está trabajando.
    agentes: Mutex<HashMap<i64, Senal>>,
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
    /// Cuántos agentes se lanzaron en este intento.
    pub lanzamientos: i32,
    /// Lo último que dijo el agente al terminar.
    pub informe: Option<String>,
    /// El commit del que partió la copia del código.
    pub desde_version: Option<String>,
    pub rama: Option<String>,
}

/// Lo que hay para hacer con una mejora.
#[derive(Clone, Debug)]
pub enum Trabajo {
    /// Recién aceptada: armar la copia del código y dejar el encargo.
    Preparar(Mejora),
    /// El agente terminó: revisar, probar, medir e instalar.
    Terminar(Mejora),
}

/// La última señal de un agente. El reloj es el de esta Mac despierta: lo
/// que duerme la máquina no cuenta como silencio.
#[derive(Clone, Debug)]
struct Senal {
    /// La marca que dejó el mod la última vez que avisó.
    marca: String,
    /// Desde cuándo se ve esa misma marca.
    desde: Instant,
    /// Desde cuándo trabaja este agente.
    empezo: Instant,
}

/// Por qué se suelta a un agente que no terminó.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Corte {
    SinSenal,
    Largo,
}

impl Senal {
    fn nueva(marca: &str, ahora: Instant) -> Self {
        Self { marca: marca.to_owned(), desde: ahora, empezo: ahora }
    }

    /// Anota la marca que hay ahora y dice si hay que soltar al agente.
    fn mirar(&mut self, marca: &str, ahora: Instant) -> Option<Corte> {
        if self.marca != marca {
            marca.clone_into(&mut self.marca);
            self.desde = ahora;
        }
        if ahora.duration_since(self.empezo) > LIMITE_AGENTE {
            Some(Corte::Largo)
        } else if ahora.duration_since(self.desde) > SIN_SENAL {
            Some(Corte::SinSenal)
        } else {
            None
        }
    }
}

impl Corte {
    fn motivo(self) -> String {
        match self {
            Self::SinSenal => {
                "La conversación de Claude donde trabajaba el agente se cerró antes de que terminara.".to_owned()
            }
            Self::Largo => format!("El agente trabajó más de {} minutos sin terminar.", LIMITE_AGENTE.as_secs() / 60),
        }
    }
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

impl From<Falla> for Desenlace {
    fn from(Falla { motivo, puntajes, rama }: Falla) -> Self {
        Self::Fallida { motivo, puntajes, rama }
    }
}

/// Lee una mejora con lo que hace falta para trabajarla.
async fn leer<'e, E>(conexion: E, id: i64) -> anyhow::Result<Mejora>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let f = sqlx::query(
        "select m.id, m.resumen, m.detalle, m.ejemplo, m.grupo_id, m.toca_base, m.borra_datos,
                m.suma_librerias, m.intentos, m.lanzamientos, m.informe, m.desde_version, m.rama,
                g.empresa, g.proyecto
         from mejoras m left join grupos g on g.id = m.grupo_id
         where m.id = ?1",
    )
    .bind(id)
    .fetch_one(conexion)
    .await?;
    let empresa: Option<String> = f.get("empresa");
    let proyecto: Option<String> = f.get("proyecto");
    let ejemplo: String = f.get("ejemplo");
    Ok(Mejora {
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
        lanzamientos: f.get("lanzamientos"),
        informe: f.get("informe"),
        desde_version: f.get("desde_version"),
        rama: f.get("rama"),
    })
}

impl Guardian {
    #[must_use]
    pub fn nuevo(cfg: Config, pool: SqlitePool) -> Self {
        Self { cfg, pool, instalacion: Mutex::new(()), agentes: Mutex::new(HashMap::new()) }
    }

    /// Lo que había quedado a medias de una corrida anterior del guardián:
    /// lo que estaba preparando se prepara de nuevo, lo que estaba probando
    /// se prueba de nuevo, y lo que espera a su agente sigue esperando si su
    /// copia del código sigue ahí.
    pub async fn recuperar(&self) -> anyhow::Result<u64> {
        const A_LA_FILA: &str = "update mejoras set
               estado = 'aceptada', fase = null, fase_desde = null, empezada = null,
               intentos = max(intentos - 1, 0)";
        let mut retomadas = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{A_LA_FILA} where estado = 'en_curso' and (fase is null or fase = 'preparando')"
        )))
        .execute(&self.pool)
        .await?
        .rows_affected();
        retomadas +=
            sqlx::query("update mejoras set fase = 'agente_listo' where estado = 'en_curso' and fase = 'probando'")
                .execute(&self.pool)
                .await?
                .rows_affected();
        let con_copia: Vec<i64> = sqlx::query_scalar("select id from mejoras where estado = 'en_curso' order by id")
            .fetch_all(&self.pool)
            .await?;
        for id in con_copia {
            if !git::copia_de(&self.cfg, id).is_dir() {
                retomadas += sqlx::query(sqlx::AssertSqlSafe(format!("{A_LA_FILA} where id = ?1")))
                    .bind(id)
                    .execute(&self.pool)
                    .await?
                    .rows_affected();
            }
        }
        Ok(retomadas)
    }

    /// Toma lo próximo que hay para hacer, si hay: primero una mejora cuyo
    /// agente ya terminó; si no, una recién aceptada.
    pub async fn tomar(&self) -> anyhow::Result<Option<Trabajo>> {
        // Con el turno de escritura tomado desde el principio, dos tareas
        // nunca se llevan la misma mejora.
        let mut tx = self.pool.begin_with("begin immediate").await?;
        let ahora = chrono::Utc::now();
        let lista: Option<i64> = sqlx::query_scalar(
            "select id from mejoras where estado = 'en_curso' and fase = 'agente_listo'
             order by fase_desde, id limit 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = lista {
            sqlx::query("update mejoras set fase = 'probando', fase_desde = ?2 where id = ?1")
                .bind(id)
                .bind(ahora)
                .execute(&mut *tx)
                .await?;
            let mejora = leer(&mut *tx, id).await?;
            tx.commit().await?;
            return Ok(Some(Trabajo::Terminar(mejora)));
        }
        let aceptada: Option<i64> =
            sqlx::query_scalar("select id from mejoras where estado = 'aceptada' order by creada, id limit 1")
                .fetch_optional(&mut *tx)
                .await?;
        let Some(id) = aceptada else {
            return Ok(None);
        };
        sqlx::query(
            "update mejoras set
               estado = 'en_curso', fase = 'preparando', fase_desde = ?2, empezada = ?2,
               intentos = intentos + 1, lanzamientos = 0, encargo = null, informe = null,
               desde_version = null, agente_sesion = null, agente_visto = null
             where id = ?1",
        )
        .bind(id)
        .bind(ahora)
        .execute(&mut *tx)
        .await?;
        let mejora = leer(&mut *tx, id).await?;
        tx.commit().await?;
        Ok(Some(Trabajo::Preparar(mejora)))
    }

    /// Suma el ejemplo del reporte a la batería, antes de que el agente
    /// empiece. Devuelve el nombre de esa pregunta, si el reporte traía qué
    /// se buscó y qué notas tenían que aparecer. Llamarla de nuevo no la
    /// repite: devuelve el mismo nombre.
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

    /// Primer tramo: arma la copia del código y deja el encargo. Desde acá
    /// la mejora espera que una conversación de Claude lance al agente.
    async fn preparar(&self, m: &Mejora) -> Result<(), Falla> {
        let cfg = &self.cfg;
        let base_git = git::version_instalada(cfg).await?;
        self.sumar_a_la_bateria(m).await?;
        let (dir, rama) = git::copia_nueva(cfg, m.id, m.intentos, &base_git).await?;
        // Con su ruta de verdad, que es como el agente va a nombrar sus archivos.
        let dir = dir.canonicalize().unwrap_or(dir);
        let encargo = Encargo::armar(cfg, m, &dir)?;
        let encargo = serde_json::to_string(&encargo).context("no pude escribir el encargo")?;
        let listas = sqlx::query(
            "update mejoras set
               fase = 'esperando_agente', fase_desde = ?2, encargo = ?3, desde_version = ?4, rama = ?5,
               agente_sesion = null, agente_visto = null
             where id = ?1 and estado = 'en_curso' and fase = 'preparando'",
        )
        .bind(m.id)
        .bind(chrono::Utc::now())
        .bind(encargo)
        .bind(&base_git)
        .bind(&rama)
        .execute(&self.pool)
        .await
        .context("no pude dejar el encargo")?
        .rows_affected();
        if listas == 0 {
            return Err("La mejora dejó de estar en curso mientras se armaba su copia del código.".to_owned().into());
        }
        tracing::info!(
            mejora = m.id,
            intento = m.intentos,
            base = %base_git,
            "copia lista: espera que una conversación de Claude lance al agente"
        );
        Ok(())
    }

    /// Último tramo: revisa lo que dejó el agente, compila, prueba, mide e instala.
    async fn terminar(&self, m: &Mejora) -> Result<Desenlace, Falla> {
        let cfg = &self.cfg;
        let dir = git::copia_de(cfg, m.id);
        let base_git = m.desde_version.clone().context("no quedó anotado de qué versión partió la copia")?;
        let rama = m.rama.clone().unwrap_or_else(|| git::rama_de(m.id, m.intentos));
        let Some(informe) = m.informe.as_deref().map(str::trim).filter(|i| !i.is_empty()) else {
            return Err("El agente terminó sin decir qué hizo.".to_owned().into());
        };
        if !dir.is_dir() {
            return Err("La copia del código donde trabajó el agente ya no está.".to_owned().into());
        }
        // Si otra mejora se instaló mientras el agente trabajaba, no vale la
        // pena probar esta: se rehace sobre la versión nueva.
        if git::version_instalada(cfg).await? != base_git {
            return Ok(Desenlace::Reintentar);
        }
        let nueva = self.sumar_a_la_bateria(m).await?;
        git::deshacer_guardado(cfg, &dir, &base_git).await?;
        let cambios = git::cambios(cfg, &dir).await?;
        if cambios.is_empty() {
            return Err(format!("El agente revisó y no cambió nada. {informe}").into());
        }
        revision::revisar_rutas(&cambios, m.permisos)?;
        let mut con_migracion = false;
        for cambio in cambios.iter().filter(|c| c.ruta.starts_with("migraciones/")) {
            let contenido = std::fs::read_to_string(dir.join(&cambio.ruta)).unwrap_or_default();
            revision::revisar_migracion(&cambio.ruta, &contenido, m.permisos)?;
            con_migracion = true;
        }
        tracing::info!(mejora = m.id, archivos = cambios.len(), "el agente terminó: se prueba su cambio");
        self.compilar_y_probar(m, &dir, con_migracion).await?;

        let mensaje = format!(
            "Mejora {}: {}\n\n{informe}\n\nHecha por el agente de mantenimiento de la memoria y revisada por el guardián.",
            m.id, m.resumen
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
            cambios: informe.to_owned(),
            antes: corto(&base_git),
            despues: corto(&commit),
            puntajes,
            rama,
        })
    }

    /// Saca todo lo que se armó para una mejora. Lo compilado de una que se
    /// instaló queda como punto de partida de la próxima.
    async fn limpiar(&self, id: i64, instalada: bool) {
        let cfg = &self.cfg;
        let copia = git::copia_de(cfg, id);
        let compilados = git::compilados_de(&copia);
        if instalada && compilados.is_dir() {
            let _ = std::fs::remove_dir_all(git::compilados_base(cfg));
            let _ = std::fs::rename(&compilados, git::compilados_base(cfg));
        }
        let _ = std::fs::remove_dir_all(&compilados);
        let _ = std::fs::remove_file(encargo::cargo_de(&copia));
        git::quitar_copia(cfg, &copia).await;
        let _ = std::fs::remove_file(candidato(cfg, id));
        let _ = base::borrar(cfg, &foto_de_la_base(cfg, id));
        let _ = base::borrar(cfg, &cfg.trabajo().join(format!("mejora-{id}-migracion.db")));
        for momento in ["antes", "despues"] {
            let _ = std::fs::remove_dir_all(cfg.trabajo().join(format!("mejora-{id}-{momento}")));
        }
    }

    /// Limpia lo armado y deja anotado cómo terminó.
    async fn cerrar(&self, m: &Mejora, desenlace: Desenlace) {
        // Una que se rehace sobre la versión nueva arma su copia otra vez.
        self.limpiar(m.id, matches!(desenlace, Desenlace::Instalada { .. })).await;
        if let Err(falla) = self.anotar(m, &desenlace).await {
            tracing::error!(mejora = m.id, falla = format!("{falla:#}"), "no pude anotar cómo terminó");
        }
    }

    /// Hace el tramo que toca. El futuro va en una caja con su tipo dicho a
    /// mano: así el compilador revisa acá, con los préstamos a la vista, que
    /// se puede mandar a otra tarea.
    pub fn atender<'a>(&'a self, trabajo: &'a Trabajo) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            match trabajo {
                Trabajo::Preparar(m) => {
                    if let Err(falla) = self.preparar(m).await {
                        self.cerrar(m, falla.into()).await;
                    }
                }
                Trabajo::Terminar(m) => {
                    let desenlace = self.terminar(m).await.unwrap_or_else(Desenlace::from);
                    self.cerrar(m, desenlace).await;
                }
            }
        })
    }

    /// Mira cómo les va a los agentes. Al que avisó que no pudo, al que dejó
    /// de dar señales (se cerró su conversación) y al que lleva demasiado, lo
    /// suelta: si quedan lanzamientos arma la copia de nuevo para otro
    /// agente; si no, la mejora termina sin instalarse.
    pub async fn revisar_agentes(&self) -> anyhow::Result<()> {
        let filas = sqlx::query(
            "select id, fase, agente_visto, motivo from mejoras
             where estado = 'en_curso' and fase in ('con_agente', 'agente_fallo')
             order by id",
        )
        .fetch_all(&self.pool)
        .await?;
        let ahora = Instant::now();
        let mut trabajando = Vec::new();
        for f in filas {
            let id: i64 = f.get("id");
            let fase: String = f.get("fase");
            let motivo = if fase == "con_agente" {
                trabajando.push(id);
                let marca: Option<String> = f.get("agente_visto");
                let marca = marca.unwrap_or_default();
                let corte = self
                    .agentes
                    .lock()
                    .await
                    .entry(id)
                    .or_insert_with(|| Senal::nueva(&marca, ahora))
                    .mirar(&marca, ahora);
                match corte {
                    Some(corte) => corte.motivo(),
                    None => continue,
                }
            } else {
                let motivo: Option<String> = f.get("motivo");
                motivo.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| "El agente no pudo terminar.".to_owned())
            };
            if let Err(falla) = self.soltar_agente(id, &fase, &motivo).await {
                tracing::warn!(mejora = id, falla = format!("{falla:#}"), "no pude soltar al agente");
            }
        }
        self.agentes.lock().await.retain(|id, _| trabajando.contains(id));
        Ok(())
    }

    /// Le saca la mejora a un agente que no terminó y decide qué sigue.
    async fn soltar_agente(&self, id: i64, fase: &str, motivo: &str) -> anyhow::Result<()> {
        self.agentes.lock().await.remove(&id);
        // Desde acá la mejora vuelve a ser del guardián: el mod ya no puede
        // anotar nada de ese agente.
        let tomada = sqlx::query(
            "update mejoras set fase = 'preparando', fase_desde = ?3, agente_sesion = null, agente_visto = null
             where id = ?1 and estado = 'en_curso' and fase = ?2",
        )
        .bind(id)
        .bind(fase)
        .bind(chrono::Utc::now())
        .execute(&self.pool)
        .await?
        .rows_affected();
        if tomada == 0 {
            // Justo terminó, o ya no está en curso: no hay nada que soltar.
            return Ok(());
        }
        let m = leer(&self.pool, id).await?;
        if m.lanzamientos >= LANZAMIENTOS {
            tracing::warn!(mejora = id, motivo, lanzamientos = m.lanzamientos, "el agente no terminó: no se lanza más");
            let motivo = format!("El agente no llegó a terminar en {} intentos. {motivo}", m.lanzamientos);
            self.cerrar(&m, Desenlace::Fallida { motivo, puntajes: None, rama: None }).await;
            return Ok(());
        }
        tracing::info!(
            mejora = id,
            motivo,
            lanzamientos = m.lanzamientos,
            "el agente no terminó: se arma la copia de nuevo para lanzar otro"
        );
        if let Err(falla) = self.preparar(&m).await {
            self.cerrar(&m, falla.into()).await;
        }
        Ok(())
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
                sqlx::query(
                    "update mejoras set estado = 'aceptada', empezada = null, fase = null, fase_desde = null,
                            encargo = null, agente_sesion = null, agente_visto = null
                     where id = ?1",
                )
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
                    terminada = ?10, avisada = 0, fase = null, fase_desde = null, encargo = null,
                    agente_sesion = null, agente_visto = null
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
    use std::process::Command;

    use super::*;

    const BUSCAR: &str = "crates/servicio/src/buscar.rs";

    /// Una instalación de prueba: la base con su estructura de verdad y una
    /// copia del código, que es un repositorio de git mínimo.
    struct Ensayo {
        dir: PathBuf,
        guardian: Guardian,
        /// El commit del que salió «lo instalado».
        version: String,
    }

    impl Ensayo {
        async fn nuevo(nombre: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("memoria-guardian-{nombre}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap();
            let cfg = Config::de_prueba(&dir);

            let opciones = SqliteConnectOptions::new().filename(&cfg.bd).create_if_missing(true).foreign_keys(true);
            let pool = SqlitePoolOptions::new().connect_with(opciones).await.unwrap();
            let migraciones = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migraciones");
            Migrator::new(migraciones).await.unwrap().run(&pool).await.unwrap();

            std::fs::create_dir_all(cfg.repo.join("crates/servicio/src")).unwrap();
            std::fs::write(cfg.repo.join(BUSCAR), "pub fn buscar() {}\n").unwrap();
            std::fs::write(cfg.repo.join("README.md"), "# Prueba\n").unwrap();
            let mut ensayo = Self { guardian: Guardian::nuevo(cfg, pool), dir, version: String::new() };
            ensayo.git(&["init", "--quiet", "--initial-branch", "instalada"]);
            ensayo.version = ensayo.guardar_en_el_codigo("inicial");
            ensayo
        }

        fn cfg(&self) -> &Config {
            &self.guardian.cfg
        }

        /// `git` en la copia del código, sin los ajustes de quien corre las pruebas.
        fn git(&self, args: &[&str]) -> String {
            let salida = Command::new("git")
                .args(["-c", "user.name=prueba", "-c", "user.email=prueba@example.invalid"])
                .args(args)
                .current_dir(&self.cfg().repo)
                .env_clear()
                .envs(self.cfg().entorno())
                .output()
                .unwrap();
            assert!(salida.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&salida.stderr));
            String::from_utf8_lossy(&salida.stdout).trim().to_owned()
        }

        /// Guarda lo que haya en la copia del código y lo deja como «lo instalado».
        fn guardar_en_el_codigo(&self, mensaje: &str) -> String {
            self.git(&["add", "-A"]);
            self.git(&["commit", "--quiet", "-m", mensaje]);
            let version = self.git(&["rev-parse", "HEAD"]);
            git::anotar_instalada(self.cfg(), &version).unwrap();
            version
        }

        async fn mejora(&self, estado: &str, fase: Option<&str>) -> i64 {
            sqlx::query_scalar(
                "insert into mejoras (resumen, detalle, firma, origen, ejemplo, estado, fase, creada)
                 values ('La búsqueda no trae el acuerdo.', 'Busqué y no apareció.', 'acuerdo', 'claude', '{}', ?1, ?2, ?3)
                 returning id",
            )
            .bind(estado)
            .bind(fase)
            .bind(chrono::Utc::now())
            .fetch_one(&self.guardian.pool)
            .await
            .unwrap()
        }

        async fn estado(&self, id: i64) -> (String, Option<String>) {
            sqlx::query_as("select estado, fase from mejoras where id = ?1")
                .bind(id)
                .fetch_one(&self.guardian.pool)
                .await
                .unwrap()
        }

        async fn texto(&self, id: i64, columna: &str) -> Option<String> {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select {columna} from mejoras where id = ?1")))
                .bind(id)
                .fetch_one(&self.guardian.pool)
                .await
                .unwrap()
        }

        /// Una mejora aceptada que el guardián ya preparó: devuelve su id y
        /// la carpeta de la copia donde va a trabajar el agente.
        async fn esperando_agente(&self) -> (i64, PathBuf) {
            let id = self.mejora("aceptada", None).await;
            self.esperando_agente_de(id).await
        }

        /// El guardián toma la mejora aceptada `id` y le hace el primer tramo.
        async fn esperando_agente_de(&self, id: i64) -> (i64, PathBuf) {
            let trabajo = self.guardian.tomar().await.unwrap().expect("hay una mejora para preparar");
            assert!(matches!(&trabajo, Trabajo::Preparar(m) if m.id == id), "{trabajo:?}");
            self.guardian.atender(&trabajo).await;
            assert_eq!(self.estado(id).await, ("en_curso".into(), Some("esperando_agente".into())));
            let encargo: Value = serde_json::from_str(&self.texto(id, "encargo").await.unwrap()).unwrap();
            (id, PathBuf::from(encargo["copia"].as_str().unwrap()))
        }

        /// Lo que anota el servicio cuando una conversación toma la mejora.
        async fn con_agente(&self, id: i64, lanzamientos: i32) {
            sqlx::query(
                "update mejoras set fase = 'con_agente', fase_desde = ?2, agente_sesion = 'local_a',
                        agente_visto = ?2, lanzamientos = ?3
                 where id = ?1",
            )
            .bind(id)
            .bind(chrono::Utc::now())
            .bind(lanzamientos)
            .execute(&self.guardian.pool)
            .await
            .unwrap();
        }

        /// Lo que anota el servicio cuando el mod avisa que el agente no pudo.
        async fn agente_fallo(&self, id: i64, motivo: &str) {
            sqlx::query("update mejoras set fase = 'agente_fallo', motivo = ?2 where id = ?1")
                .bind(id)
                .bind(motivo)
                .execute(&self.guardian.pool)
                .await
                .unwrap();
        }

        /// El agente terminó con `informe`: el guardián toma la mejora y hace
        /// su último tramo. Devuelve cómo quedó y el motivo, si lo hay.
        async fn terminar_con(&self, id: i64, informe: &str) -> (String, Option<String>) {
            self.con_agente(id, 1).await;
            sqlx::query("update mejoras set fase = 'agente_listo', informe = ?2 where id = ?1")
                .bind(id)
                .bind(informe)
                .execute(&self.guardian.pool)
                .await
                .unwrap();
            let trabajo = self.guardian.tomar().await.unwrap().expect("hay una mejora para terminar");
            assert!(matches!(&trabajo, Trabajo::Terminar(m) if m.id == id), "{trabajo:?}");
            self.guardian.atender(&trabajo).await;
            (self.estado(id).await.0, self.texto(id, "motivo").await)
        }
    }

    impl Drop for Ensayo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn toma_primero_lo_que_dejo_un_agente_y_retoma_lo_que_quedo_a_medias() {
        let e = Ensayo::nuevo("turnos").await;
        let en = |estado: &str, fase: Option<&str>| (estado.to_owned(), fase.map(str::to_owned));
        let aceptada = e.mejora("aceptada", None).await;
        let lista = e.mejora("en_curso", Some("agente_listo")).await;
        let primero = e.guardian.tomar().await.unwrap();
        assert!(matches!(&primero, Some(Trabajo::Terminar(m)) if m.id == lista), "{primero:?}");
        assert_eq!(e.estado(lista).await, en("en_curso", Some("probando")));
        let segundo = e.guardian.tomar().await.unwrap();
        assert!(matches!(&segundo, Some(Trabajo::Preparar(m)) if m.id == aceptada && m.intentos == 1), "{segundo:?}");
        assert_eq!(e.estado(aceptada).await, en("en_curso", Some("preparando")));
        assert!(e.guardian.tomar().await.unwrap().is_none());

        // El guardián se corta y vuelve a arrancar.
        let esperando = e.mejora("en_curso", Some("esperando_agente")).await;
        let sin_copia = e.mejora("en_curso", Some("con_agente")).await;
        for id in [lista, esperando] {
            std::fs::create_dir_all(git::copia_de(e.cfg(), id)).unwrap();
        }
        assert_eq!(e.guardian.recuperar().await.unwrap(), 3);
        assert_eq!(e.estado(aceptada).await, en("aceptada", None), "lo que se estaba preparando se prepara de nuevo");
        assert_eq!(e.texto(aceptada, "cast(intentos as text)").await.as_deref(), Some("0"));
        assert_eq!(e.estado(lista).await, en("en_curso", Some("agente_listo")), "lo que se probaba se prueba de nuevo");
        assert_eq!(
            e.estado(esperando).await,
            en("en_curso", Some("esperando_agente")),
            "la que espera sigue esperando"
        );
        assert_eq!(e.estado(sin_copia).await, en("aceptada", None), "sin su copia del código, empieza de nuevo");
    }

    #[tokio::test]
    async fn prepara_la_copia_y_lanza_otro_agente_si_el_primero_no_termina() {
        let e = Ensayo::nuevo("agentes").await;
        let (id, copia) = e.esperando_agente().await;
        let encargo: Value = serde_json::from_str(&e.texto(id, "encargo").await.unwrap()).unwrap();
        let cargo = PathBuf::from(encargo["cargo"].as_str().unwrap());
        assert_eq!(std::fs::read_to_string(copia.join(BUSCAR)).unwrap(), "pub fn buscar() {}\n");
        assert_eq!(cargo, encargo::cargo_de(&copia));
        assert!(cargo.is_file(), "el `cargo` del agente queda al lado de la copia");
        assert!(encargo["pedido"].as_str().unwrap().contains("## El pedido\n\nMejora 1 de la memoria."));
        assert_eq!(e.texto(id, "desde_version").await.as_deref(), Some(e.version.as_str()));
        assert_eq!(e.texto(id, "rama").await.as_deref(), Some("mejora-1-1"));

        // Una conversación la toma y su agente empieza a tocar cosas.
        e.con_agente(id, 1).await;
        std::fs::write(copia.join(BUSCAR), "// a medio hacer\n").unwrap();
        std::fs::write(copia.join("crates/servicio/src/sobra.rs"), "// sobra\n").unwrap();
        // Mientras el mod avise que sigue andando, el guardián no hace nada.
        e.guardian.revisar_agentes().await.unwrap();
        assert_eq!(e.estado(id).await.1.as_deref(), Some("con_agente"));

        // El mod avisa que el agente no pudo: la copia se arma de nuevo, limpia.
        e.agente_fallo(id, "Se cortó la conexión.").await;
        e.guardian.revisar_agentes().await.unwrap();
        assert_eq!(e.estado(id).await, ("en_curso".into(), Some("esperando_agente".into())));
        assert_eq!(std::fs::read_to_string(copia.join(BUSCAR)).unwrap(), "pub fn buscar() {}\n");
        assert!(!copia.join("crates/servicio/src/sobra.rs").exists());
        assert_eq!(e.texto(id, "agente_sesion").await, None);
        assert_eq!(e.texto(id, "cast(lanzamientos as text)").await.as_deref(), Some("1"));

        // Al tercero que no termina, la mejora queda sin instalar y no queda nada armado.
        e.con_agente(id, LANZAMIENTOS).await;
        e.agente_fallo(id, "Se cortó la conexión.").await;
        e.guardian.revisar_agentes().await.unwrap();
        assert_eq!(e.estado(id).await, ("fallida".into(), None));
        assert_eq!(
            e.texto(id, "motivo").await.as_deref(),
            Some("El agente no llegó a terminar en 3 intentos. Se cortó la conexión.")
        );
        assert!(!copia.exists() && !cargo.exists());
        assert_eq!(e.texto(id, "encargo").await, None);
        assert_eq!(e.texto(id, "cast(avisada as text)").await.as_deref(), Some("0"), "falta avisar en el chat");
    }

    #[tokio::test]
    async fn revisa_lo_que_dejo_el_agente_antes_de_compilar_nada() {
        let e = Ensayo::nuevo("revision").await;
        let fallida = |motivo: &str| ("fallida".to_owned(), Some(motivo.to_owned()));

        let (id, _) = e.esperando_agente().await;
        assert_eq!(e.terminar_con(id, "  ").await, fallida("El agente terminó sin decir qué hizo."));

        let (id, copia) = e.esperando_agente().await;
        assert_eq!(
            e.terminar_con(id, "Miré y estaba bien.").await,
            fallida("El agente revisó y no cambió nada. Miré y estaba bien.")
        );
        assert!(!copia.exists(), "lo armado para una mejora que no se instaló se saca");

        let (id, copia) = e.esperando_agente().await;
        std::fs::write(copia.join(BUSCAR), "pub fn buscar() -> u8 { 1 }\n").unwrap();
        std::fs::write(copia.join("README.md"), "# Otra cosa\n").unwrap();
        assert_eq!(
            e.terminar_con(id, "Cambié la búsqueda.").await,
            fallida("El agente tocó archivos que no puede tocar: README.md.")
        );

        // Otra mejora se instaló mientras el agente trabajaba: esta vuelve a
        // la fila para rehacerse sobre la versión nueva.
        let (id, copia) = e.esperando_agente().await;
        std::fs::write(copia.join(BUSCAR), "pub fn buscar() -> u8 { 2 }\n").unwrap();
        std::fs::write(e.cfg().repo.join("README.md"), "# Con otra mejora\n").unwrap();
        let nueva = e.guardar_en_el_codigo("otra mejora");
        assert_eq!(e.terminar_con(id, "Cambié la búsqueda.").await, ("aceptada".to_owned(), None));
        assert!(!copia.exists());
        let (otra_vez, copia) = e.esperando_agente_de(id).await;
        assert_eq!(otra_vez, id);
        assert_eq!(e.texto(id, "desde_version").await.as_deref(), Some(nueva.as_str()));
        assert_eq!(e.texto(id, "rama").await.as_deref(), Some(format!("mejora-{id}-2").as_str()));
        assert_eq!(std::fs::read_to_string(copia.join("README.md")).unwrap(), "# Con otra mejora\n");
    }

    #[test]
    fn suelta_al_agente_que_deja_de_dar_senales() {
        let seg = Duration::from_secs;
        let t0 = Instant::now();
        let mut senal = Senal::nueva("12:00:00", t0);
        assert_eq!(senal.mirar("12:00:00", t0 + seg(110)), None);
        // Avisó de nuevo: el silencio se cuenta desde ahí.
        assert_eq!(senal.mirar("12:01:55", t0 + seg(115)), None);
        assert_eq!(senal.mirar("12:01:55", t0 + seg(230)), None);
        assert_eq!(senal.mirar("12:01:55", t0 + seg(236)), Some(Corte::SinSenal));
        // Uno que avisa siempre pero no termina nunca.
        let mut senal = Senal::nueva("a", t0);
        let limite = LIMITE_AGENTE.as_secs();
        assert_eq!(senal.mirar("b", t0 + seg(limite - 1)), None);
        assert_eq!(senal.mirar("c", t0 + seg(limite + 1)), Some(Corte::Largo));
        assert!(Corte::SinSenal.motivo().contains("se cerró"));
        assert!(Corte::Largo.motivo().contains("45 minutos"));
    }

    #[test]
    fn nombra_las_versiones_en_corto() {
        assert_eq!(corto("7c00fea1234567890"), "7c00fea");
    }
}
