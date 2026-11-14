//! Errores del servicio: los de uso se le muestran tal cual a Claude; los de
//! la base, resumidos.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Algo que Claude puede corregir en su pedido (falta el grupo, nota vacía…).
    #[error("{0}")]
    Uso(String),
    #[error("falla de la base: {0}")]
    Base(#[from] sqlx::Error),
}

pub type Resultado<T> = Result<T, Error>;

pub fn uso<T>(mensaje: impl Into<String>) -> Resultado<T> {
    Err(Error::Uso(mensaje.into()))
}
