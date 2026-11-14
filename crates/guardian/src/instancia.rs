//! Una copia del servicio prendida aparte (otro puerto, otra base) para
//! probarla con la batería sin tocar la que usa Claude.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tokio::process::{Child, Command};

use crate::base;
use crate::config::Config;

/// Cuánto se espera a que arranque: aplica migraciones, hace un respaldo y
/// carga las huellas.
const ARRANQUE: Duration = Duration::from_secs(120);
/// Cuánto se espera a que el modelo de huellas quede listo para medir.
const HUELLAS_LISTAS: Duration = Duration::from_secs(600);
const PEDIDO: Duration = Duration::from_secs(60);

pub struct Instancia {
    hijo: Child,
    http: reqwest::Client,
    url: String,
    clave: String,
    dir: PathBuf,
}

/// Prende `programa` en `puerto` sobre una copia propia de la base guardada
/// en `foto` (o sobre una base vacía, sin `foto`), con su carpeta en `dir`.
/// No lee conversaciones: la batería no depende de lo que se esté charlando
/// en ese momento.
pub async fn prender(
    cfg: &Config,
    programa: &Path,
    foto: Option<&Path>,
    puerto: u16,
    dir: &Path,
) -> anyhow::Result<Instancia> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("sin-conversaciones"))?;
    let bd = dir.join("memoria.db");
    if let Some(foto) = foto {
        base::copiar_desde(cfg, foto, &bd).await?;
    }
    let salida = std::fs::File::create(dir.join("salida.log"))?;
    let hijo = Command::new(programa)
        .env_clear()
        .envs(cfg.entorno().iter().map(|(clave, valor)| (*clave, valor.as_str())))
        .envs(cfg.huellas.iter().map(|(clave, valor)| (*clave, valor.as_str())))
        .env("MEMORIA_BD", &bd)
        .env("MEMORIA_DIRECCION", format!("127.0.0.1:{puerto}"))
        .env("MEMORIA_DIR", dir)
        .env("MEMORIA_REGISTROS", dir.join("registros"))
        .env("MEMORIA_CONVERSACIONES", dir.join("sin-conversaciones"))
        .stdin(Stdio::null())
        .stdout(salida.try_clone()?)
        .stderr(salida)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("no pude prender {}", programa.display()))?;
    let http = reqwest::Client::builder().timeout(PEDIDO).build()?;
    let mut instancia =
        Instancia { hijo, http, url: format!("http://127.0.0.1:{puerto}"), clave: String::new(), dir: dir.to_owned() };
    let inicio = tokio::time::Instant::now();
    loop {
        if let Some(estado) = instancia.hijo.try_wait()? {
            bail!("el servicio de prueba se cerró al arrancar ({estado}): {}", instancia.registro());
        }
        if instancia.sana().await {
            break;
        }
        if inicio.elapsed() > ARRANQUE {
            bail!("el servicio de prueba no arrancó a tiempo: {}", instancia.registro());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    instancia.clave = std::fs::read_to_string(dir.join("clave")).context("el servicio de prueba no dejó su clave")?;
    Ok(instancia)
}

impl Instancia {
    /// El archivo de la base sobre la que corre.
    #[must_use]
    pub fn bd(&self) -> PathBuf {
        self.dir.join("memoria.db")
    }

    async fn sana(&self) -> bool {
        match self.http.get(format!("{}/salud", self.url)).send().await {
            Ok(r) if r.status().is_success() => r.json::<Value>().await.is_ok_and(|v| v["ok"] == true),
            _ => false,
        }
    }

    /// Espera a que el modelo de huellas esté cargado y a que no queden
    /// huellas pendientes de calcular: medir antes daría resultados peores
    /// de lo que el programa realmente hace.
    pub async fn esperar_huellas(&self) -> anyhow::Result<()> {
        let inicio = tokio::time::Instant::now();
        loop {
            let salud: Value = self.http.get(format!("{}/salud", self.url)).send().await?.json().await?;
            if salud["huellas"] == true && salud["pendientes"].as_i64().unwrap_or(0) == 0 {
                return Ok(());
            }
            if salud["huellas"] == false && inicio.elapsed() > Duration::from_secs(30) {
                bail!("el servicio de prueba no puede calcular huellas: {}", self.registro());
            }
            if inicio.elapsed() > HUELLAS_LISTAS {
                bail!("las huellas del servicio de prueba no quedaron listas a tiempo: {salud}");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Lo último que dijo el servicio de prueba, para explicar una falla.
    fn registro(&self) -> String {
        let texto = std::fs::read_to_string(self.dir.join("salida.log")).unwrap_or_default();
        let total = texto.chars().count();
        texto.chars().skip(total.saturating_sub(1500)).collect()
    }

    /// Llama a una herramienta de la memoria como la llama Claude y devuelve
    /// su texto. Un error de la herramienta es un error acá.
    pub async fn llamar(&self, herramienta: &str, argumentos: Value) -> anyhow::Result<String> {
        let pedido = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": herramienta, "arguments": argumentos },
        });
        let respuesta: Value = self
            .http
            .post(format!("{}/mcp", self.url))
            .bearer_auth(self.clave.trim())
            .header("Accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-06-18")
            .json(&pedido)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let resultado = &respuesta["result"];
        let texto = resultado["content"][0]["text"].as_str().unwrap_or_default().to_owned();
        if resultado["isError"] == true || resultado.is_null() {
            bail!(
                "{herramienta} respondió con un error: {}",
                if texto.is_empty() { respuesta.to_string() } else { texto }
            );
        }
        Ok(texto)
    }

    pub async fn apagar(mut self) {
        if let Some(pid) = self.hijo.id() {
            // Primero se le pide que cierre bien; si no, se lo corta.
            let _ = Command::new("/bin/kill").args(["-TERM", &pid.to_string()]).status().await;
        }
        if tokio::time::timeout(Duration::from_secs(10), self.hijo.wait()).await.is_err() {
            let _ = self.hijo.kill().await;
        }
    }
}
