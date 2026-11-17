//! Slack como fuente, contra un Slack de mentira que atiende en esta máquina:
//! qué se le pide, qué se guarda, qué queda afuera, cómo se sigue desde donde
//! se había llegado y qué pasa cuando una llave no anda.

mod comun;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{NaiveDate, NaiveTime};
use comun::estado;
use memoria::config::{Espacio, Fuentes, Llave, Slack};
use memoria::estilo::fuentes::{self, Fuente};
use memoria::estilo::slack::{self, Juntado, Lectura};
use memoria::mcp::{self, PedidoEstilo};
use serde_json::{Value, json};
use sqlx::SqlitePool;

/// Las llaves de dos espacios de trabajo de mentira.
const LLAVE_DE_ACME: &str = "xoxp-de-mentira-acme";
const LLAVE_DE_GLOBEX: &str = "xoxp-de-mentira-globex";
/// Quién es el usuario en esos espacios.
const YO: &str = "U0ANA";
const LUIS: &str = "U0LUIS";
/// 2026-11-10T12:00:00Z.
const AHORA: i64 = 1_794_312_000;

/// Un Slack de mentira: tiene los mensajes de cada espacio (por su llave),
/// anota lo que se le pide y puede pedir que vayan más despacio.
#[derive(Default)]
struct Falso {
    espacios: Mutex<HashMap<String, Vec<Value>>>,
    pedidos: Mutex<Vec<(String, HashMap<String, String>)>>,
    /// Cuántas búsquedas más contesta con «más despacio».
    frenos: AtomicUsize,
    /// De a cuántos mensajes contesta cada búsqueda.
    por_pagina: usize,
}

impl Falso {
    fn nuevo(por_pagina: usize) -> Arc<Self> {
        Arc::new(Self { por_pagina, ..Self::default() })
    }

    fn sumar(&self, llave: &str, mensajes: Vec<Value>) {
        self.espacios.lock().unwrap().entry(llave.to_owned()).or_default().extend(mensajes);
    }

    /// Los mensajes del espacio de quien pide, si su llave vale.
    fn del_que_pide(&self, encabezados: &HeaderMap) -> Option<Vec<Value>> {
        let llave = encabezados.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")?;
        self.espacios.lock().unwrap().get(llave).cloned()
    }

    fn anotar(&self, metodo: &str, datos: &HashMap<String, String>) {
        self.pedidos.lock().unwrap().push((metodo.to_owned(), datos.clone()));
    }

    /// Lo que se le pidió de un método, en orden.
    fn pedidos_de(&self, metodo: &str) -> Vec<HashMap<String, String>> {
        self.pedidos.lock().unwrap().iter().filter(|(m, _)| m == metodo).map(|(_, datos)| datos.clone()).collect()
    }
}

fn rechazo() -> Response {
    Json(json!({"ok": false, "error": "invalid_auth"})).into_response()
}

fn momento(mensaje: &Value) -> f64 {
    mensaje["ts"].as_str().unwrap().parse().unwrap()
}

async fn quien_soy(State(falso): State<Arc<Falso>>, encabezados: HeaderMap) -> Response {
    falso.anotar("auth.test", &HashMap::new());
    if falso.del_que_pide(&encabezados).is_none() {
        return rechazo();
    }
    Json(json!({"ok": true, "user": "ana", "user_id": YO, "team": "Un espacio"})).into_response()
}

/// La búsqueda: los mensajes de quien se pide (y uno ajeno que se cuela),
/// desde el día que sigue al de `after:`, del más viejo al más nuevo y de a
/// páginas.
#[expect(clippy::cast_precision_loss, reason = "son fechas de este siglo")]
async fn buscar(
    State(falso): State<Arc<Falso>>,
    encabezados: HeaderMap,
    Query(datos): Query<HashMap<String, String>>,
) -> Response {
    falso.anotar("search.messages", &datos);
    if falso.frenos.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_ok() {
        return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "0")], "").into_response();
    }
    let Some(mensajes) = falso.del_que_pide(&encabezados) else { return rechazo() };
    let consulta = datos.get("query").cloned().unwrap_or_default();
    let autor = consulta.split_whitespace().find_map(|parte| parte.strip_prefix("from:<@")?.strip_suffix('>')).unwrap();
    let desde = consulta.split_whitespace().find_map(|parte| parte.strip_prefix("after:")).map_or(0.0, |dia| {
        let dia = NaiveDate::parse_from_str(dia, "%Y-%m-%d").unwrap().succ_opt().unwrap();
        dia.and_time(NaiveTime::MIN).and_utc().timestamp() as f64
    });
    let mut suyos: Vec<Value> =
        mensajes.into_iter().filter(|m| (m["user"] == autor || m["colado"] == true) && momento(m) >= desde).collect();
    suyos.sort_by(|a, b| momento(a).total_cmp(&momento(b)));
    let total = suyos.len();
    let primero = match datos.get("cursor").map(String::as_str) {
        None | Some("*") => 0,
        Some(cursor) => cursor.parse::<usize>().unwrap(),
    };
    let cuantos = if datos.get("count").map(String::as_str) == Some("1") { 1 } else { falso.por_pagina };
    let pagina: Vec<Value> = suyos.into_iter().skip(primero).take(cuantos).collect();
    let ultimo = primero + pagina.len();
    let sigue = if ultimo < total { ultimo.to_string() } else { String::new() };
    Json(json!({
        "ok": true,
        "query": consulta,
        "messages": {"total": total, "matches": pagina, "pagination": {"next_cursor": sigue}},
    }))
    .into_response()
}

async fn persona(
    State(falso): State<Arc<Falso>>,
    encabezados: HeaderMap,
    Query(datos): Query<HashMap<String, String>>,
) -> Response {
    falso.anotar("users.info", &datos);
    if falso.del_que_pide(&encabezados).is_none() {
        return rechazo();
    }
    let respuesta = match datos.get("user").map(String::as_str) {
        Some(LUIS) => json!({"ok": true, "user": {
            "id": LUIS, "name": "luis.gomez", "real_name": "Luis Gómez",
            "profile": {"real_name": "Luis Gómez", "display_name": "luisg"},
        }}),
        _ => json!({"ok": false, "error": "user_not_found"}),
    };
    Json(respuesta).into_response()
}

/// Pone a atender un Slack de mentira y devuelve dónde.
async fn atender(falso: Arc<Falso>) -> String {
    let rutas = Router::new()
        .route("/auth.test", get(quien_soy))
        .route("/search.messages", get(buscar))
        .route("/users.info", get(persona))
        .with_state(falso);
    let escucha = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direccion = escucha.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(escucha, rutas).await.unwrap();
    });
    format!("http://{direccion}")
}

fn canal(id: &str, nombre: &str) -> Value {
    json!({"id": id, "name": nombre, "is_channel": true, "is_im": false, "is_mpim": false, "is_private": false})
}

fn directo(id: &str, con: &str) -> Value {
    json!({"id": id, "name": con, "is_channel": false, "is_im": true, "is_mpim": false, "user": con})
}

fn grupo_chico(id: &str, nombre: &str) -> Value {
    json!({"id": id, "name": nombre, "is_channel": false, "is_im": false, "is_mpim": true})
}

/// Un mensaje de `de` en una conversación, `minuto` minutos después de [`AHORA`].
fn mensaje(de: &str, donde: &Value, minuto: i64, texto: &str) -> Value {
    json!({
        "type": "message",
        "user": de,
        "username": "alguien",
        "ts": format!("{}.{:06}", AHORA + minuto * 60, minuto),
        "text": texto,
        "channel": donde,
        "permalink": "https://example.com/archivo",
    })
}

fn con_luis() -> Value {
    directo("D0LUIS", LUIS)
}

/// Lo que hay en el espacio ACME: lo que el usuario escribió en un mensaje
/// directo, en un canal y en un grupo chico, y lo que no se tiene que leer.
fn de_acme() -> Vec<Value> {
    let equipo = canal("C0EQUIPO", "equipo-acme");
    let mut colado = mensaje(LUIS, &con_luis(), 3, "Perfecto, gracias! Cualquier cosa te aviso");
    colado["colado"] = json!(true);
    vec![
        mensaje(YO, &con_luis(), 0, "dale, mañana te mando el resumen de la reunión"),
        mensaje(YO, &con_luis(), 1, "che <@U0LUIS> pudiste ver <https://example.com/doc|el documento>?"),
        mensaje(YO, &con_luis(), 2, "&gt; ¿llegás a las 10?\nsí, llego justo. gracias :pray:"),
        // Lo que escriben los demás no entra nunca, aunque Slack lo devuelva.
        colado,
        mensaje(YO, &equipo, 4, "alguien pudo revisar lo de ayer? yo no llegue"),
        mensaje(YO, &equipo, 5, "probá con esto y avisame\n```\ncargo test\n```"),
        mensaje(YO, &grupo_chico("G0GRUPO", "mpdm-ana--luis.gomez--pedro-1"), 6, "nos juntamos el jueves a la tarde?"),
        // Lo que no es para nadie: las notas que se deja y lo que le pide al ayudante.
        mensaje(YO, &directo("D0YO", YO), 7, "nota para mi: renovar el dominio antes del viernes"),
        mensaje(YO, &directo("D0BOT", "USLACKBOT"), 8, "recordame mañana a las 9 lo del informe"),
        // Alguien de quien Slack ya no da el nombre: el mensaje vale igual.
        mensaje(YO, &directo("D0NADIE", "U0NADIE"), 9, "ok, mañana te confirmo el horario"),
        // Un canal que el usuario dejó afuera.
        mensaje(YO, &canal("C0RANDOM", "random"), 10, "alguien vio el partido de anoche?"),
    ]
}

fn espacio(nombre: &str, llave: &str) -> Espacio {
    Espacio { nombre: nombre.into(), llave: Llave::Fija(llave.into()) }
}

fn ajustes(api: &str, espacios: Vec<Espacio>) -> Slack {
    Slack { espacios, sin: vec!["random".into()], api: api.to_owned(), pausa: Duration::ZERO }
}

type Guardado = (String, String, Option<String>, Option<String>, String);

async fn guardados(pool: &SqlitePool) -> Vec<Guardado> {
    sqlx::query_as("select fuente, lugar, conversacion, para, contenido from mensajes_propios order by fecha, id")
        .fetch_all(pool)
        .await
        .unwrap()
}

fn leido(juntado: Juntado) -> Lectura {
    Lectura { juntado, espacios: 1, fallas: vec![] }
}

#[tokio::test]
async fn lee_solo_lo_que_escribio_el_usuario_y_lo_deja_como_lo_escribio() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let falso = Falso::nuevo(3);
    falso.sumar(LLAVE_DE_ACME, de_acme());
    let cfg = ajustes(&atender(falso.clone()).await, vec![espacio("acme", LLAVE_DE_ACME)]);

    // Probando: pregunta cuántos hay y no guarda nada.
    let probado = slack::juntar(&pool, &cfg, "Ana", false).await;
    assert_eq!(probado, leido(Juntado { leidos: 11, ..Juntado::default() }));
    assert_eq!(guardados(&pool).await.len(), 0);
    assert_eq!(falso.pedidos_de("search.messages").len(), 1, "probando alcanza con una búsqueda");

    let juntado = slack::juntar(&pool, &cfg, "Ana", true).await;
    assert_eq!(juntado, leido(Juntado { leidos: 7, propios: 7, nuevos: 7, chats: 4 }));
    let todos = guardados(&pool).await;
    let textos: Vec<&str> = todos.iter().map(|g| g.4.as_str()).collect();
    assert_eq!(
        textos,
        [
            "dale, mañana te mando el resumen de la reunión",
            // La mención y el enlace quedan como texto.
            "che @alguien pudiste ver el documento?",
            // Sin lo que citó del otro, y con el emoji.
            "sí, llego justo. gracias 🙏",
            "alguien pudo revisar lo de ayer? yo no llegue",
            // Sin el código que pegó.
            "probá con esto y avisame",
            "nos juntamos el jueves a la tarde?",
            "ok, mañana te confirmo el horario",
        ]
    );
    // De cada conversación queda de qué espacio es y a quién le escribía.
    let de: Vec<(&str, Option<&str>)> = todos.iter().map(|g| (g.2.as_deref().unwrap(), g.3.as_deref())).collect();
    assert_eq!(de[0], ("slack:acme:D0LUIS", Some("Luis Gómez")));
    assert_eq!(de[3], ("slack:acme:C0EQUIPO", Some("#equipo-acme (acme)")));
    assert_eq!(de[5], ("slack:acme:G0GRUPO", Some("luis.gomez, pedro")));
    assert_eq!(de[6], ("slack:acme:D0NADIE", None));
    assert!(todos.iter().all(|g| (g.0.as_str(), g.1.as_str()) == ("slack", "slack")));

    // Lo que se le pidió a Slack: lo del usuario, del más viejo al más nuevo,
    // página por página; y el nombre de cada persona, una sola vez.
    let busquedas = falso.pedidos_de("search.messages");
    assert_eq!(busquedas.len(), 1 + 4, "la de prueba y cuatro páginas de a tres");
    assert_eq!(busquedas[1]["query"], format!("from:<@{YO}>"));
    assert_eq!((busquedas[1]["sort"].as_str(), busquedas[1]["sort_dir"].as_str()), ("timestamp", "asc"));
    let cursores: Vec<&str> = busquedas[1..].iter().map(|b| b["cursor"].as_str()).collect();
    assert_eq!(cursores, ["*", "3", "6", "9"]);
    let personas: Vec<String> = falso.pedidos_de("users.info").into_iter().map(|p| p["user"].clone()).collect();
    assert_eq!(personas, [LUIS, "U0NADIE"]);

    // La llave no queda en ningún lado de la base.
    let escrito: Vec<String> = sqlx::query_scalar(
        "select contenido || ' ' || coalesce(conversacion, '') || ' ' || coalesce(para, '') from mensajes_propios
         union all select origen from lecturas_propias
         union all select clave || ' ' || valor from ajustes",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(escrito.len() > 7 && !escrito.iter().any(|texto| texto.contains("xoxp")), "{escrito:?}");

    // Sin nada nuevo no guarda nada, y pide solo lo de los últimos días.
    assert_eq!(slack::juntar(&pool, &cfg, "Ana", true).await, leido(Juntado::default()));
    let ultima = falso.pedidos_de("search.messages").pop().unwrap();
    assert_eq!(ultima["query"], format!("from:<@{YO}> after:2026-11-08"));
    // Lo que escribe después se lee desde ahí.
    falso.sumar(LLAVE_DE_ACME, vec![mensaje(YO, &con_luis(), 100, "listo, ya te lo mande por mail")]);
    let despues = slack::juntar(&pool, &cfg, "Ana", true).await;
    assert_eq!(despues, leido(Juntado { leidos: 1, propios: 1, nuevos: 1, chats: 1 }));
    assert_eq!(guardados(&pool).await.last().unwrap().4, "listo, ya te lo mande por mail");
}

#[tokio::test]
async fn un_espacio_que_no_se_puede_leer_no_frena_a_los_demas() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let falso = Falso::nuevo(100);
    falso.sumar(LLAVE_DE_ACME, de_acme());
    let api = atender(falso.clone()).await;

    // Si Slack pide ir más despacio, se espera y se vuelve a pedir.
    falso.frenos.store(2, Ordering::Relaxed);
    let cfg = ajustes(&api, vec![espacio("acme", LLAVE_DE_ACME)]);
    assert_eq!(slack::juntar(&pool, &cfg, "Ana", true).await.juntado.nuevos, 7);
    assert_eq!(falso.pedidos_de("search.messages").len(), 3, "dos veces frenado y la que anduvo");

    // Una llave anulada y otra que no está en el llavero: se dice cuál y por qué, sin mostrar ninguna.
    let cfg = ajustes(
        &api,
        vec![
            espacio("globex", "xoxp-de-mentira-anulada"),
            espacio("acme", LLAVE_DE_ACME),
            Espacio { nombre: "inicial".into(), llave: Llave::Llavero("memoria-prueba-que-no-existe".into()) },
        ],
    );
    falso.sumar(LLAVE_DE_ACME, vec![mensaje(YO, &con_luis(), 200, "al final lo dejamos para la semana que viene")]);
    let lectura = slack::juntar(&pool, &cfg, "Ana", true).await;
    assert_eq!((lectura.espacios, lectura.juntado.nuevos), (1, 1), "{lectura:?}");
    assert_eq!(lectura.fallas.len(), 2, "{lectura:?}");
    assert!(lectura.fallas[0].starts_with("«globex»: la llave no sirve o fue anulada"), "{lectura:?}");
    assert!(lectura.fallas[1].starts_with("«inicial»: ") && lectura.fallas[1].contains("llavero"), "{lectura:?}");
    assert!(!format!("{lectura:?} {cfg:?}").contains("xoxp"), "una llave quedó a la vista");

    // Quien pregunta cómo escribe se entera, y no se insiste hasta el otro día.
    let mut e = estado(pool.clone()).await;
    let encendidas = Fuentes { whatsapp: None, slack: Some(cfg) };
    e.fuentes = Arc::new(encendidas.clone());
    let (bien, resumen) = fuentes::juntar(&pool, &encendidas, "Ana").await.unwrap();
    assert!(
        !bien && resumen.starts_with("Slack: 0 mensajes nuevos (de 0 tuyos en 0 conversaciones de 1 espacio"),
        "{resumen}"
    );
    assert!(resumen.contains(" No se pudo leer «globex»: la llave no sirve o fue anulada"), "{resumen}");
    assert!(!fuentes::toca(&pool, &encendidas).await.unwrap());
    let r = mcp::estilo(&e, &PedidoEstilo { lugar: Some("slack".into()), ..PedidoEstilo::default() }).await.unwrap();
    assert!(r.contains("Slack está encendido, pero la última lectura (") && r.contains("no anduvo"), "{r}");
    assert!(r.contains("«globex»: la llave no sirve o fue anulada"), "{r}");
}

#[tokio::test]
async fn se_lee_cuando_toca_y_cada_fuente_lleva_su_cuenta() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let mut e = estado(pool.clone()).await;
    let falso = Falso::nuevo(100);
    falso.sumar(LLAVE_DE_ACME, de_acme());
    let cfg = ajustes(&atender(falso.clone()).await, vec![espacio("acme", LLAVE_DE_ACME)]);
    let encendidas = Fuentes { whatsapp: None, slack: Some(cfg) };
    e.fuentes = Arc::new(encendidas.clone());

    assert!(fuentes::toca(&pool, &encendidas).await.unwrap());
    // Probar no guarda ni cuenta como lectura.
    let (bien, resumen) = fuentes::probar(&pool, &encendidas, "Ana").await;
    assert!(bien && resumen == "Slack: hay 11 mensajes tuyos para leer en 1 espacio. No guardé nada.", "{resumen}");
    assert!(fuentes::toca(&pool, &encendidas).await.unwrap());

    let (bien, resumen) = fuentes::juntar(&pool, &encendidas, "Ana").await.unwrap();
    assert!(bien, "{resumen}");
    assert_eq!(resumen, "Slack: 7 mensajes nuevos (de 7 tuyos en 4 conversaciones de 1 espacio que había para leer).");
    // Recién leído no toca de nuevo, y no hay nada que avisar.
    assert!(!fuentes::toca(&pool, &encendidas).await.unwrap());
    assert_eq!(fuentes::aviso(&e).await.unwrap(), None);
    // Lo de Slack queda anotado como de Slack: WhatsApp sigue sin haberse leído nunca.
    assert!(fuentes::ultima(&pool, Fuente::Slack).await.unwrap().is_some_and(|ultima| ultima.bien));
    assert_eq!(fuentes::ultima(&pool, Fuente::WhatsApp).await.unwrap(), None);
}

/// Lo que el usuario le escribió a Luis, de sobra para mostrar ejemplos.
const A_LUIS: [&str; 6] = [
    "dale, mañana te mando el resumen de la reunión",
    "che pudiste ver lo que te pase ayer?",
    "no llego para las 10, arranquen sin mi",
    "ok despues lo vemos con mas tiempo",
    "tenes el link de la reunion? no lo encuentro",
    "mañana no puedo, lo pasamos para el jueves?",
];

#[tokio::test]
async fn lo_de_cada_persona_y_de_cada_canal_se_muestra_solo_a_quien_corresponde() {
    let base = comun::Base::nueva().await;
    let e = estado(base.pool.clone()).await;
    let falso = Falso::nuevo(100);
    let general = canal("C0GENERAL", "general");
    let mut de_acme: Vec<Value> =
        A_LUIS.iter().zip(0..).map(|(texto, minuto)| mensaje(YO, &con_luis(), minuto, texto)).collect();
    de_acme.push(mensaje(YO, &general, 20, "buen día, hoy arranco un poco más tarde"));
    de_acme.push(mensaje(YO, &general, 21, "ya subí el informe de la semana al tablero"));
    falso.sumar(LLAVE_DE_ACME, de_acme);
    // En el otro espacio hay un canal que se llama igual.
    falso.sumar(LLAVE_DE_GLOBEX, vec![mensaje(YO, &general, 30, "aviso que mañana no estoy en todo el día")]);
    let cfg = ajustes(
        &atender(falso.clone()).await,
        vec![espacio("acme", LLAVE_DE_ACME), espacio("globex", LLAVE_DE_GLOBEX)],
    );
    let lectura = slack::juntar(&e.pool, &cfg, "Ana", true).await;
    assert_eq!((lectura.espacios, lectura.juntado.nuevos, lectura.juntado.chats), (2, 9, 3), "{lectura:?}");

    let pedir = |para: Option<&str>| PedidoEstilo {
        lugar: Some("Slack".into()),
        para: para.map(Into::into),
        ejemplos: Some(15),
        ..PedidoEstilo::default()
    };
    // Sin decir a quién: lo medido sí, los mensajes no.
    let r = mcp::estilo(&e, &pedir(None)).await.unwrap();
    assert!(r.starts_with("Así escribe el usuario en slack · español (9 mensajes suyos"), "{r}");
    assert!(r.contains("No muestro mensajes suyos de ejemplo") && !r.contains("resumen de la reunión"), "{r}");

    // A una persona: solo lo que le escribió a ella.
    let r = mcp::estilo(&e, &pedir(Some("Luis"))).await.unwrap();
    assert!(r.contains("· a Luis Gómez] «dale, mañana te mando el resumen de la reunión»"), "{r}");
    assert!(r.contains("A «Luis Gómez» le escribió poco (6 mensajes)"), "{r}");
    assert!(!r.contains("#general") && !r.contains("hoy arranco") && !r.contains("no estoy"), "{r}");

    // El mismo canal en dos espacios son dos: no elige, y no muestra nada de ninguno.
    let r = mcp::estilo(&e, &pedir(Some("general"))).await.unwrap();
    assert!(r.contains("«general» coincide con varios chats suyos: «#general (acme)», «#general (globex)»."), "{r}");
    assert!(!r.contains("· a ") && !r.contains("hoy arranco") && !r.contains("no estoy"), "{r}");
    // Diciendo de qué espacio, sí: y lo de un espacio no aparece en el otro.
    let r = mcp::estilo(&e, &pedir(Some("general acme"))).await.unwrap();
    assert!(r.contains("· a #general (acme)] «buen día, hoy arranco un poco más tarde»"), "{r}");
    assert!(!r.contains("no estoy en todo el día") && !r.contains("Luis Gómez"), "{r}");
    let r = mcp::estilo(&e, &pedir(Some("#general (globex)"))).await.unwrap();
    assert!(r.contains("· a #general (globex)] «aviso que mañana no estoy en todo el día»"), "{r}");
    assert!(!r.contains("hoy arranco") && !r.contains("informe de la semana"), "{r}");
}
