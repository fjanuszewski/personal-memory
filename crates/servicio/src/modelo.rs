//! Tipos de nota y sus estados, igual que los `enum` de la base.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TipoNota {
    /// Algo que se decidió, con el porqué.
    Decision,
    /// Un error con su causa y su solución.
    Error,
    /// Un cambio importante que se hizo.
    Cambio,
    /// Algo acordado con el cliente.
    Acuerdo,
    /// Un hecho útil.
    Dato,
    /// Algo que quedó por hacer.
    Pendiente,
}

impl TipoNota {
    pub const TODOS: [Self; 6] =
        [Self::Decision, Self::Error, Self::Cambio, Self::Acuerdo, Self::Dato, Self::Pendiente];

    /// El nombre en la base (`decision`).
    #[must_use]
    pub const fn clave(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Error => "error",
            Self::Cambio => "cambio",
            Self::Acuerdo => "acuerdo",
            Self::Dato => "dato",
            Self::Pendiente => "pendiente",
        }
    }
}

/// Cómo se lee (`decisión`).
impl fmt::Display for TipoNota {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Decision => "decisión",
            other => other.clave(),
        })
    }
}

impl FromStr for TipoNota {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_lowercase();
        let s = if s == "decisión" { "decision" } else { s.as_str() };
        Self::TODOS.into_iter().find(|t| t.clave() == s).ok_or_else(|| format!("tipo de nota desconocido: {s}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EstadoNota {
    Vigente,
    Reemplazada,
    Descartada,
}

impl FromStr for EstadoNota {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "vigente" => Ok(Self::Vigente),
            "reemplazada" => Ok(Self::Reemplazada),
            "descartada" => Ok(Self::Descartada),
            otro => Err(format!("estado de nota desconocido: {otro}")),
        }
    }
}

impl fmt::Display for EstadoNota {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Vigente => "vigente",
            Self::Reemplazada => "reemplazada",
            Self::Descartada => "descartada",
        })
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn lee_los_tipos_con_y_sin_acento() {
        assert_eq!("decisión".parse::<TipoNota>(), Ok(TipoNota::Decision));
        assert_eq!("Decision".parse::<TipoNota>(), Ok(TipoNota::Decision));
        assert_eq!("pendiente".parse::<TipoNota>(), Ok(TipoNota::Pendiente));
        assert!("idea".parse::<TipoNota>().is_err());
        assert_eq!(TipoNota::Decision.to_string(), "decisión");
    }
}
