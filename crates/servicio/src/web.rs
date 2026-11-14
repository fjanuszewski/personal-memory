//! El servidor HTTP: `/mcp` para Claude (con la clave local) y `/salud`
//! para el guardián y la instalación.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::clave;
use crate::estado::Estado;
use crate::mcp::Memoria;

/// Arma las rutas. Sin sesiones de MCP guardadas en memoria: cada pedido se
/// atiende solo, así un reinicio del servicio no corta a las conversaciones.
pub fn rutas(estado: Estado, clave: &str, apagado: CancellationToken) -> Router {
    let estado_mcp = estado.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(Memoria::nueva(estado_mcp.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None)
            .with_cancellation_token(apagado),
    );
    let clave: Arc<str> = Arc::from(clave);
    let protegidas = Router::new().nest_service("/mcp", mcp).layer(middleware::from_fn_with_state(clave, exigir_clave));
    Router::new().route("/salud", get(salud)).with_state(estado).merge(protegidas)
}

async fn exigir_clave(State(esperada): State<Arc<str>>, pedido: Request, siguiente: Next) -> Response {
    let dada = pedido
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .trim();
    if clave::iguales(dada.as_bytes(), esperada.as_bytes()) {
        siguiente.run(pedido).await
    } else {
        (StatusCode::UNAUTHORIZED, "Falta la clave local de memoria.").into_response()
    }
}

/// `ok` depende solo de la base: sin huellas la memoria sigue andando, por
/// palabras. `huellas` es `null` hasta el primer pedido. `pendientes` es
/// cuántas notas y pedazos de conversación esperan todavía su huella.
async fn salud(State(e): State<Estado>) -> (StatusCode, Json<Value>) {
    let pendientes: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "select (select count(*) from notas where huella is null and estado <> 'descartada')
              + (select count(*) from fragmentos where huella is null)",
    )
    .fetch_one(&e.pool)
    .await;
    let base = pendientes.is_ok();
    let codigo = if base { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    let cuerpo = json!({
        "ok": base,
        "version": env!("CARGO_PKG_VERSION"),
        "base": base,
        "huellas": e.huellas_andan(),
        "modelo_huellas": e.huellas.modelo(),
        "pendientes": pendientes.ok(),
    });
    (codigo, Json(cuerpo))
}
