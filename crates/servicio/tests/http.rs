//! El servidor HTTP de punta a punta (sin abrir puertos): la clave, `/salud`
//! y las herramientas por MCP como las llama Claude Code. Más los respaldos
//! y las migraciones.

mod comun;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use memoria::config::Config;
use memoria::modelo::TipoNota;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const CLAVE: &str = "clave-de-prueba-0123456789abcdef0123456789";

async fn rutas(pool: SqlitePool) -> Router {
    memoria::web::rutas(comun::estado(pool).await, CLAVE, CancellationToken::new())
}

async fn pedir(
    rutas: &Router,
    metodo: &str,
    ruta: &str,
    clave: Option<&str>,
    cuerpo: Option<Value>,
) -> (StatusCode, String) {
    let mut p = Request::builder()
        .method(metodo)
        .uri(ruta)
        .header(header::HOST, "127.0.0.1:7337")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18");
    if let Some(c) = clave {
        p = p.header(header::AUTHORIZATION, format!("Bearer {c}"));
    }
    let p = match cuerpo {
        Some(v) => p.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())),
        None => p.body(Body::empty()),
    }
    .unwrap();
    let r = rutas.clone().oneshot(p).await.unwrap();
    let estado = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (estado, String::from_utf8_lossy(&bytes).into_owned())
}

async fn rpc(rutas: &Router, id: u32, metodo: &str, params: Value) -> Value {
    let (estado, cuerpo) = pedir(
        rutas,
        "POST",
        "/mcp",
        Some(CLAVE),
        Some(json!({ "jsonrpc": "2.0", "id": id, "method": metodo, "params": params })),
    )
    .await;
    assert_eq!(estado, StatusCode::OK, "{cuerpo}");
    serde_json::from_str(&cuerpo).unwrap_or_else(|e| panic!("{e}: {cuerpo}"))
}

fn texto_de(r: &Value) -> &str {
    r["result"]["content"][0]["text"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn salud_responde_sin_clave() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let (estado, cuerpo) = pedir(&rutas(pool).await, "GET", "/salud", None, None).await;
    assert_eq!(estado, StatusCode::OK);
    let v: Value = serde_json::from_str(&cuerpo).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["base"], true);
    assert_eq!(v["modelo_huellas"], "de-palabras");
    assert!(v["huellas"].is_null(), "todavía no se pidió ninguna huella: {v}");
}

#[tokio::test]
async fn mcp_pide_la_clave() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let r = rutas(pool).await;
    let lista = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    let (sin, _) = pedir(&r, "POST", "/mcp", None, Some(lista.clone())).await;
    assert_eq!(sin, StatusCode::UNAUTHORIZED);
    let (mala, _) = pedir(&r, "POST", "/mcp", Some("otra"), Some(lista)).await;
    assert_eq!(mala, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn claude_registra_guarda_y_encuentra_por_mcp() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let r = rutas(pool).await;
    let inicio = rpc(
        &r,
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "claude-code", "version": "2" }
        }),
    )
    .await;
    assert_eq!(inicio["result"]["serverInfo"]["name"], "memoria");
    assert!(inicio["result"]["instructions"].as_str().unwrap().contains("memoria_buscar"));

    let lista = rpc(&r, 2, "tools/list", json!({})).await;
    let mut nombres: Vec<&str> =
        lista["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    nombres.sort_unstable();
    assert_eq!(
        nombres,
        [
            "memoria_buscar",
            "memoria_contexto",
            "memoria_guardar",
            "memoria_mejora_estado",
            "memoria_mejorar",
            "memoria_rechazar",
            "memoria_reemplazar",
            "memoria_reportar"
        ]
    );

    // Lo que hace el mod al empezar.
    let contexto = rpc(
        &r,
        3,
        "tools/call",
        json!({ "name": "memoria_contexto", "arguments": {
            "sesion": "local_1",
            "registro": { "empresa": "ACME", "proyecto": "Portal", "grupo_app": "[ACME] - PORTAL", "carpeta": "/x/portal" }
        }}),
    )
    .await;
    assert!(texto_de(&contexto).starts_with("Grupo: ACME › Portal"), "{contexto}");

    let guardada = rpc(
        &r,
        4,
        "tools/call",
        json!({ "name": "memoria_guardar", "arguments": {
            "sesion": "local_1", "tipo": "decision",
            "titulo": "Paginación con cursor", "cuerpo": "Qué: la API pagina con cursor. Por qué: offset era lento."
        }}),
    )
    .await;
    assert_eq!(texto_de(&guardada), "Guardada como nota 1 en ACME › Portal (decisión).");

    let hallada = rpc(
        &r,
        5,
        "tools/call",
        json!({ "name": "memoria_buscar", "arguments": { "sesion": "local_1", "consulta": "paginacion" } }),
    )
    .await;
    assert!(texto_de(&hallada).contains("[nota 1] decisión · vigente"), "{hallada}");

    // Una nota que corrige a la anterior, y la herramienta para dejarla reemplazada.
    let otra = rpc(
        &r,
        6,
        "tools/call",
        json!({ "name": "memoria_guardar", "arguments": {
            "sesion": "local_1", "tipo": "decision",
            "titulo": "Paginación con offset en los reportes", "cuerpo": "Qué: los reportes vuelven a offset."
        }}),
    )
    .await;
    assert!(texto_de(&otra).starts_with("Guardada como nota 2"), "{otra}");
    let reemplazo =
        rpc(&r, 7, "tools/call", json!({ "name": "memoria_reemplazar", "arguments": { "vieja": 1, "nueva": 2 } }))
            .await;
    assert!(
        texto_de(&reemplazo).starts_with("La nota 1 («Paginación con cursor») quedó como reemplazada"),
        "{reemplazo}"
    );

    // Un error de uso vuelve como error de la herramienta, con el motivo.
    let mal = rpc(
        &r,
        8,
        "tools/call",
        json!({ "name": "memoria_guardar", "arguments": { "tipo": "dato", "titulo": "x", "cuerpo": "y" } }),
    )
    .await;
    assert_eq!(mal["result"]["isError"], true);
    assert!(texto_de(&mal).starts_with("Falta el grupo"), "{mal}");
}

fn config_de_prueba(base: &comun::Base) -> Config {
    let dir = base.carpeta.clone();
    Config {
        bd: base.archivo(),
        direccion: "127.0.0.1:0".parse().unwrap(),
        registros: dir.join("registros"),
        dir,
        dias_respaldo: 14,
        nombre: "Usuario".into(),
        huellas: memoria::config::Proveedor::Ollama,
        modelo_huellas: "de-palabras".into(),
        ollama: "http://127.0.0.1:9".into(),
        modelos: base.carpeta.join("modelos"),
        hilos: 1,
        umbrales: None,
        conversaciones: std::env::temp_dir().join("memoria-sin-conversaciones"),
    }
}

#[tokio::test]
async fn respalda_una_vez_por_dia() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let cfg = config_de_prueba(&base);
    let e = comun::estado(pool.clone()).await;
    comun::empezar(&e, "local_a", "ACME", Some("Portal")).await;
    let pedido = comun::nota(TipoNota::Dato, "Región de los deploys", "Los deploys van a us-east-1.", "local_a");
    memoria::mcp::guardar(&e, pedido).await.unwrap();

    let hoy = chrono::Local::now().date_naive();
    let archivo = memoria::respaldo::diario_si_falta(&pool, &cfg, hoy).await.unwrap().expect("respaldo nuevo");
    assert!(memoria::respaldo::diario_si_falta(&pool, &cfg, hoy).await.unwrap().is_none());
    // El respaldo es una base entera: se abre sola y tiene lo guardado.
    let copia = memoria::base::abrir(&archivo).await.unwrap();
    let titulo: String = sqlx::query_scalar("select titulo from notas").fetch_one(&copia).await.unwrap();
    assert_eq!(titulo, "Región de los deploys");
    let palabras: i64 = sqlx::query_scalar("select count(*) from notas_texto where notas_texto match 'deploys'")
        .fetch_one(&copia)
        .await
        .unwrap();
    assert_eq!(palabras, 1, "el índice de palabras va en la copia");
    copia.close().await;
    // Un respaldo de la víspera más viejo que el plazo se borra al hacer el de hoy.
    let viejo = cfg.respaldos().join(memoria::respaldo::nombre_diario(hoy - chrono::Days::new(20)));
    std::fs::write(&viejo, b"x").unwrap();
    let manana = hoy + chrono::Days::new(1);
    memoria::respaldo::diario_si_falta(&pool, &cfg, manana).await.unwrap().expect("respaldo del otro día");
    assert!(!viejo.exists());
}

#[tokio::test]
async fn sin_migraciones_pendientes_no_toca_nada() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let cfg = config_de_prueba(&base);
    memoria::migrar(&pool, &cfg).await.unwrap();
    assert!(!cfg.respaldos().exists(), "no hacía falta respaldo");
}
