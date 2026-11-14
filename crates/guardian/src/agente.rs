//! El agente de mantenimiento: una sesión de Claude Code sin pantalla que
//! trabaja en la copia del código, con permisos limitados.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::config::Config;
use crate::{git, orden};

/// Las reglas fijas del agente. Están en el guardián para que el agente no
/// las pueda cambiar.
pub const INSTRUCCIONES: &str = include_str!("../instrucciones.md");

/// Lo único que el agente puede usar: leer, editar el servicio y las
/// migraciones, y los comandos de `cargo`.
const PERMITIDAS: [&str; 12] = [
    "Read",
    "Glob",
    "Grep",
    "Edit(crates/servicio/**)",
    "Write(crates/servicio/**)",
    "Edit(migraciones/**)",
    "Write(migraciones/**)",
    "Bash(cargo build:*)",
    "Bash(cargo check:*)",
    "Bash(cargo test:*)",
    "Bash(cargo clippy:*)",
    "Bash(cargo fmt:*)",
];
const PROHIBIDAS: [&str; 4] = ["WebSearch", "WebFetch", "Agent", "NotebookEdit"];
const SIN_SERVIDORES: &str = r#"{"mcpServers":{}}"#;

pub type Futuro<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Lo que dejó dicho el agente al terminar: es lo que el usuario lee en el chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Informe {
    pub resumen: String,
}

/// Quien hace el cambio en la copia del código.
pub trait Agente: Send + Sync {
    /// Trabaja en `dir` según `pedido`. Para compilar y probar no necesita
    /// ninguna base: las pruebas del servicio arman la suya.
    fn trabajar<'a>(&'a self, dir: &'a Path, pedido: &'a str) -> Futuro<'a, anyhow::Result<Informe>>;
}

pub struct ClaudeCode {
    cfg: Config,
}

#[derive(Deserialize)]
struct Respuesta {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
}

impl ClaudeCode {
    #[must_use]
    pub fn nuevo(cfg: Config) -> Self {
        Self { cfg }
    }

    fn entorno(&self, dir: &Path) -> Vec<(&'static str, String)> {
        let mut entorno = self.cfg.entorno();
        entorno.push(("CARGO_TARGET_DIR", git::compilados_de(dir).display().to_string()));
        // Con una llave guardada, el agente no depende de que la sesión de la
        // terminal siga vigente.
        if let Ok(llave) = std::fs::read_to_string(self.cfg.llave_agente())
            && !llave.trim().is_empty()
        {
            entorno.push(("CLAUDE_CODE_OAUTH_TOKEN", llave.trim().to_owned()));
        }
        entorno
    }
}

/// Lee lo que imprime `claude -p --output-format json`.
fn leer(salida: &str) -> anyhow::Result<Informe> {
    let respuesta: Respuesta = serde_json::from_str(salida.trim()).with_context(|| {
        format!("no entendí la respuesta del agente: {}", salida.chars().take(300).collect::<String>())
    })?;
    let texto = respuesta.result.unwrap_or_default().trim().to_owned();
    if respuesta.is_error {
        if texto.contains("authenticate") || texto.contains("OAuth") || texto.contains("login") {
            bail!(
                "El agente no pudo entrar a Claude: su sesión está vencida. Hay que correr instalacion/conectar-agente.sh una vez."
            );
        }
        bail!("El agente terminó con un error: {texto}");
    }
    if texto.is_empty() {
        bail!("El agente terminó sin decir qué hizo.");
    }
    Ok(Informe { resumen: texto })
}

impl Agente for ClaudeCode {
    fn trabajar<'a>(&'a self, dir: &'a Path, pedido: &'a str) -> Futuro<'a, anyhow::Result<Informe>> {
        Box::pin(async move {
            let mut args = vec!["-p", pedido, "--append-system-prompt", INSTRUCCIONES, "--allowedTools"];
            args.extend(PERMITIDAS);
            args.push("--disallowedTools");
            args.extend(PROHIBIDAS);
            args.extend([
                "--strict-mcp-config",
                "--mcp-config",
                SIN_SERVIDORES,
                "--setting-sources",
                "project",
                "--max-turns",
                "200",
                "--output-format",
                "json",
            ]);
            let entorno = self.entorno(dir);
            let salida = orden::correr(&self.cfg.agente, &args, dir, &entorno, self.cfg.limite_agente).await?;
            if salida.salida.trim().is_empty() {
                bail!("El agente no respondió: {}", salida.cola());
            }
            leer(&salida.salida)
        })
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn lee_la_respuesta_del_agente() {
        let bien = r#"{"type":"result","subtype":"success","is_error":false,"result":" Bajé el piso de parecido.\n","num_turns":12}"#;
        assert_eq!(leer(bien).unwrap().resumen, "Bajé el piso de parecido.");
        let vencida =
            r#"{"is_error":true,"result":"Failed to authenticate: OAuth session expired and could not be refreshed"}"#;
        assert!(leer(vencida).unwrap_err().to_string().contains("conectar-agente.sh"));
        let otro = r#"{"is_error":true,"result":"Reached max turns"}"#;
        assert!(leer(otro).unwrap_err().to_string().contains("Reached max turns"));
        assert!(leer(r#"{"is_error":false,"result":"  "}"#).is_err());
        assert!(leer("esto no es json").is_err());
    }

    #[test]
    fn las_reglas_nombran_lo_que_no_se_toca() {
        for protegido in ["crates/guardian/", "bateria/", "instalacion/", "mod/"] {
            assert!(INSTRUCCIONES.contains(protegido), "{protegido}");
        }
        assert!(PERMITIDAS.iter().all(|p| !p.contains("guardian") && !p.contains("bateria")));
    }
}
