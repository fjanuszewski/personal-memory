//! Slack leído por la conexión de Claude: el servicio le dice al mod qué
//! pedir y recibe lo que la conexión contestó. Acá el mod es la prueba, y lo
//! que «contesta la conexión» tiene la misma forma que lo de verdad.

mod comun;

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use comun::estado;
use memoria::config::{API_DE_SLACK, Espacio, Fuentes, Llave, Slack};
use memoria::estado::Estado;
use memoria::estilo::conexion::PasoDeFuente;
use memoria::estilo::fuentes;
use memoria::mcp::{self, PedidoEstado, PedidoEstilo};
use serde_json::{Value, json};
use sqlx::SqlitePool;

const YO: &str = "U0ANA";
/// 2026-11-10T12:00:00Z.
const AHORA: i64 = 1_794_312_000;

fn sin_llave(nombre: &str) -> Espacio {
    Espacio { nombre: nombre.into(), llave: Llave::Conexion }
}

/// Slack encendido con espacios que se leen por la conexión de Claude.
fn encendidas(espacios: Vec<Espacio>) -> Fuentes {
    let slack = Slack { espacios, sin: vec!["random".into()], api: API_DE_SLACK.to_owned(), pausa: Duration::ZERO };
    Fuentes { whatsapp: None, slack: Some(slack) }
}

async fn con_fuentes(pool: SqlitePool, fuentes: Fuentes) -> Estado {
    let mut e = estado(pool).await;
    e.fuentes = Arc::new(fuentes);
    e
}

/// Lo que hace el mod: le pasa algo al servicio y recibe qué pedirle a la conexión.
async fn pedir(e: &Estado, sesion: &str, paso: PasoDeFuente) -> Value {
    let pedido = PedidoEstado { sesion: Some(sesion.into()), fuente: Some(paso), ..PedidoEstado::default() };
    let respuesta: Value = serde_json::from_str(&mcp::mejora_estado(e, &pedido).await.unwrap()).unwrap();
    assert_eq!(respuesta.as_object().unwrap().len(), 1, "solo lo de la fuente: {respuesta}");
    respuesta["fuente"].clone()
}

/// Una conversación que tiene la conexión de Slack y no trae nada.
fn con_slack() -> PasoDeFuente {
    PasoDeFuente { conexiones: vec!["slack".into()], ..PasoDeFuente::default() }
}

fn con_pagina(espacio: &str, pagina: String) -> PasoDeFuente {
    PasoDeFuente { espacio: Some(espacio.into()), pagina: Some(pagina), ..con_slack() }
}

fn con_perfil(espacio: &str) -> PasoDeFuente {
    let perfil =
        json!({ "result": format!("User ID: {YO}\nUsername: ana\nReal Name: Ana Ruiz\nEmail: ana@example.com\n") });
    PasoDeFuente { espacio: Some(espacio.into()), perfil: Some(perfil.to_string()), ..con_slack() }
}

/// Un resultado como los que da la conexión. `donde` es «DM (ID: D0LUIS)»,
/// «#canal (ID: C0…)» o «Group DM (ID: C0…)».
fn resultado(donde: &str, participantes: Option<&str>, autor: &str, minuto: i64, texto: &str) -> String {
    let participantes = participantes.map(|p| format!("Participants: {p}\n")).unwrap_or_default();
    format!(
        "Channel: {donde}\n{participantes}From: Alguien <alguien@example.com> (ID: {autor}) \nTime: 2026-11-10 09:00:00 -03\n\
         Message_ts: {}.{minuto:06}\nPermalink: [link](https://acme.example.com/archives/X/p1)\nText: \n{texto}\n\n---\n\n",
        AHORA + minuto * 60
    )
}

/// Una página de la búsqueda, con el cursor de la que sigue si hay.
fn pagina(resultados: &[String], cursor: Option<&str>) -> String {
    let mut texto = format!("# Search Results for: \n\n## Messages ({} results)\n", resultados.len());
    for (i, resultado) in resultados.iter().enumerate() {
        let _ = write!(texto, "### Result {} of {}\n{resultado}", i + 1, resultados.len());
    }
    let paginado = cursor.map(|c| format!("For the next page of results use cursor `{c}`\n")).unwrap_or_default();
    json!({ "results": texto, "pagination_info": paginado }).to_string()
}

const CON_LUIS: Option<&str> = Some("Luis Gómez (ID: U0LUIS), Ana Ruiz (ID: U0ANA)");

type Guardado = (Option<String>, Option<String>, String);

async fn guardados(pool: &SqlitePool) -> Vec<Guardado> {
    sqlx::query_as(
        "select conversacion, para, contenido from mensajes_propios where fuente = 'slack' order by fecha, id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn ultima_lectura(pool: &SqlitePool, espacio: &str) -> Option<Value> {
    let valor: Option<String> = sqlx::query_scalar("select valor from ajustes where clave = ?1")
        .bind(format!("juntada:conexion:{espacio}"))
        .fetch_optional(pool)
        .await
        .unwrap();
    valor.map(|v| serde_json::from_str(&v).unwrap())
}

/// Hace de cuenta que la última lectura de un espacio fue hace `horas`.
async fn envejecer(pool: &SqlitePool, espacio: &str, horas: i64) {
    let mut lectura = ultima_lectura(pool, espacio).await.unwrap();
    lectura["cuando"] = json!(chrono::Utc::now() - chrono::Duration::hours(horas));
    sqlx::query("update ajustes set valor = ?1 where clave = ?2")
        .bind(lectura.to_string())
        .bind(format!("juntada:conexion:{espacio}"))
        .execute(pool)
        .await
        .unwrap();
}

/// La primera lectura de un espacio, de punta a punta, contada como la hace el
/// mod: sin la conexión, con ella, el perfil, dos páginas y la búsqueda que ya
/// no trae nada nuevo.
async fn primera_lectura(e: &Estado) {
    // Una conversación sin la conexión de Slack no tiene nada que traer.
    assert_eq!(pedir(e, "s1", PasoDeFuente::default()).await, Value::Null);
    // La que la tiene recibe el primer pedido: quién es el usuario en ese espacio.
    assert_eq!(pedir(e, "s1", con_slack()).await, json!({ "espacio": "acme", "pedir": "perfil" }));
    // Mientras lee una, a las demás no se les pide nada, y lo que traigan no vale.
    assert_eq!(pedir(e, "s2", con_slack()).await, Value::Null);
    let ajena = pagina(&[resultado("DM (ID: D0LUIS)", CON_LUIS, YO, 0, "esto lo trae quien no tiene el turno")], None);
    assert_eq!(pedir(e, "s2", con_pagina("acme", ajena)).await, Value::Null);
    assert_eq!(guardados(&e.pool).await.len(), 0);

    // Con el perfil, pide la búsqueda desde el principio.
    let busqueda = pedir(e, "s1", con_perfil("acme")).await;
    assert_eq!(
        busqueda,
        json!({ "espacio": "acme", "pedir": "buscar", "usuario": YO, "despues": null, "cursor": null, "pausa_ms": 1500 })
    );

    // Primera página: un mensaje directo, uno en un canal y uno ajeno que se coló.
    let primera = pagina(
        &[
            resultado("DM (ID: D0LUIS)", CON_LUIS, YO, 0, "dale, mañana te mando el resumen de la reunión"),
            resultado(
                "#equipo-acme (ID: C0EQUIPO)",
                None,
                YO,
                1,
                "che <@U0LUIS> pudiste ver <https://example.com/doc|el documento>?",
            ),
            resultado("DM (ID: D0LUIS)", CON_LUIS, "U0LUIS", 2, "Perfecto, gracias! Cualquier cosa te aviso"),
        ],
        Some("Q1VSU09S"),
    );
    let sigue = pedir(e, "s1", con_pagina("acme", primera)).await;
    assert_eq!((sigue["pedir"].as_str(), sigue["cursor"].as_str()), (Some("buscar"), Some("Q1VSU09S")), "{sigue}");
    assert_eq!(sigue["despues"], Value::Null, "el mismo pedido, en la página que sigue");

    // Segunda y última: lo que no es para nadie, un grupo y un canal que quedó afuera.
    let segunda = pagina(
        &[
            resultado(
                "DM (ID: D0YO)",
                Some("Ana Ruiz (ID: U0ANA), Ana Ruiz (ID: U0ANA)"),
                YO,
                3,
                "nota para mi: renovar el dominio antes del viernes",
            ),
            resultado(
                "DM (ID: D0BOT)",
                Some("Slackbot (ID: USLACKBOT), Ana Ruiz (ID: U0ANA)"),
                YO,
                4,
                "recordame mañana a las 9 lo del informe",
            ),
            resultado(
                "Group DM (ID: C0GRUPO)",
                Some("Luis Gómez (ID: U0LUIS), Marta Ruiz (ID: U0MARTA), Ana Ruiz (ID: U0ANA)"),
                YO,
                5,
                "&gt; ¿llegás a las 10?\nsí, llego justo. gracias :pray:",
            ),
            resultado("#random (ID: C0RANDOM)", None, YO, 6, "alguien vio el partido de anoche?"),
        ],
        None,
    );
    // La conexión no dio más páginas, pero corta cada búsqueda antes del final:
    // como se avanzó, se busca de nuevo desde el último mensaje visto…
    let ultimo = (AHORA + 6 * 60).to_string();
    let de_nuevo = pedir(e, "s1", con_pagina("acme", segunda)).await;
    assert_eq!(
        (de_nuevo["pedir"].as_str(), de_nuevo["despues"].as_str(), &de_nuevo["cursor"]),
        (Some("buscar"), Some(ultimo.as_str()), &Value::Null)
    );
    // …y recién cuando una búsqueda no trae nada más nuevo, terminó.
    let repetida =
        pagina(&[resultado("#random (ID: C0RANDOM)", None, YO, 6, "alguien vio el partido de anoche?")], None);
    assert_eq!(pedir(e, "s1", con_pagina("acme", repetida)).await, Value::Null, "terminó");

    let todos = guardados(&e.pool).await;
    let esperados = [
        (Some("slack:acme:D0LUIS"), Some("Luis Gómez"), "dale, mañana te mando el resumen de la reunión"),
        (Some("slack:acme:C0EQUIPO"), Some("#equipo-acme (acme)"), "che @alguien pudiste ver el documento?"),
        (Some("slack:acme:C0GRUPO"), Some("Luis Gómez, Marta Ruiz"), "sí, llego justo. gracias 🙏"),
    ];
    let leidos: Vec<_> = todos.iter().map(|(c, p, t)| (c.as_deref(), p.as_deref(), t.as_str())).collect();
    assert_eq!(leidos, esperados);
    let lectura = ultima_lectura(&e.pool, "acme").await.unwrap();
    assert_eq!(lectura["bien"], true);
    assert_eq!(
        lectura["resumen"],
        "Slack («acme», por la conexión de Claude): 3 mensajes nuevos (de 3 tuyos en 3 conversaciones que había para leer)."
    );

    // Recién leído no toca: a nadie se le pide nada, y no hay nada que avisar.
    assert_eq!(pedir(e, "s2", con_slack()).await, Value::Null);
    assert_eq!(fuentes::aviso(e).await.unwrap(), None);
}

#[tokio::test]
async fn el_mod_trae_de_la_conexion_lo_que_el_servicio_le_pide() {
    let base = comun::Base::nueva().await;
    let e = con_fuentes(base.pool.clone(), encendidas(vec![sin_llave("acme")])).await;
    primera_lectura(&e).await;
}

#[tokio::test]
async fn la_lectura_siguiente_sigue_desde_donde_se_llego() {
    let base = comun::Base::nueva().await;
    let e = con_fuentes(base.pool.clone(), encendidas(vec![sin_llave("acme")])).await;
    primera_lectura(&e).await;

    // Horas después le toca de nuevo: cualquier conversación sigue desde donde se llegó.
    envejecer(&e.pool, "acme", 7).await;
    let desde = (AHORA + 6 * 60).to_string();
    let otra_vez = pedir(&e, "s2", con_slack()).await;
    assert_eq!((otra_vez["pedir"].as_str(), otra_vez["despues"].as_str()), (Some("buscar"), Some(desde.as_str())));
    let nueva = pagina(
        &[
            // Uno que ya estaba leído (el pedido incluye ese segundo) y uno nuevo.
            resultado("#random (ID: C0RANDOM)", None, YO, 6, "alguien vio el partido de anoche?"),
            resultado("DM (ID: D0LUIS)", CON_LUIS, YO, 100, "listo, ya te lo mande por mail"),
            resultado("DM (ID: D0LUIS)", CON_LUIS, YO, 101, "che pudiste ver lo que te pase ayer?"),
            resultado("DM (ID: D0LUIS)", CON_LUIS, YO, 102, "mañana no puedo, lo pasamos para el jueves?"),
        ],
        None,
    );
    let sigue = pedir(&e, "s2", con_pagina("acme", nueva)).await;
    assert_eq!(sigue["despues"].as_str(), Some((AHORA + 102 * 60).to_string().as_str()), "{sigue}");
    // Lo que contesta la conexión cuando no hay nada.
    let nada = json!({ "results": "# Search Results for: \n\nNo results found.\n", "pagination_info": "End of results - No more pages available.\n" });
    assert_eq!(pedir(&e, "s2", con_pagina("acme", nada.to_string())).await, Value::Null);
    assert_eq!(guardados(&e.pool).await.len(), 6);
    assert_eq!(guardados(&e.pool).await[3].2, "listo, ya te lo mande por mail");
    let lectura = ultima_lectura(&e.pool, "acme").await.unwrap();
    assert!(
        lectura["resumen"].as_str().unwrap().contains("3 mensajes nuevos (de 3 tuyos en 1 conversaciones"),
        "{lectura}"
    );

    // Lo de cada uno se muestra solo cuando se le va a escribir a esa persona o a ese canal.
    let estilo =
        |para: &str| PedidoEstilo { lugar: Some("slack".into()), para: Some(para.into()), ..PedidoEstilo::default() };
    let r = mcp::estilo(&e, &estilo("Luis Gómez")).await.unwrap();
    assert!(r.contains("· a Luis Gómez] «dale, mañana te mando el resumen de la reunión»"), "{r}");
    assert!(!r.contains("#equipo-acme") && !r.contains("llego justo"), "{r}");
}

#[tokio::test]
async fn si_la_conexion_no_anda_queda_avisado_y_no_se_insiste() {
    let base = comun::Base::nueva().await;
    let e = con_fuentes(base.pool.clone(), encendidas(vec![sin_llave("acme")])).await;
    assert_eq!(pedir(&e, "s1", con_slack()).await["pedir"], "perfil");

    // El mod no pudo pedirle nada a la conexión: lo dice, y el servicio lo anota.
    let falla = PasoDeFuente {
        espacio: Some("acme".into()),
        falla: Some("La conexión de Slack no contestó.".into()),
        ..con_slack()
    };
    assert_eq!(pedir(&e, "s1", falla).await, Value::Null);
    let lectura = ultima_lectura(&e.pool, "acme").await.unwrap();
    assert_eq!(lectura["bien"], false);
    assert_eq!(
        lectura["resumen"],
        "Slack («acme», por la conexión de Claude): no se pudo leer. La conexión de Slack no contestó."
    );
    // No se insiste hasta el otro día, y quien pregunta cómo escribe se entera.
    assert_eq!(pedir(&e, "s1", con_slack()).await, Value::Null);
    envejecer(&e.pool, "acme", 7).await;
    assert_eq!(pedir(&e, "s1", con_slack()).await, Value::Null);
    let r = mcp::estilo(&e, &PedidoEstilo { lugar: Some("slack".into()), ..PedidoEstilo::default() }).await.unwrap();
    assert!(r.contains("Slack está encendido, pero la última lectura (") && r.contains("no anduvo"), "{r}");
    assert!(r.contains("(«acme», por la conexión de Claude): no se pudo leer."), "{r}");

    // Al otro día se prueba de nuevo. Si lo que contesta la conexión cambió de forma, no se guarda nada.
    envejecer(&e.pool, "acme", 25).await;
    assert_eq!(pedir(&e, "s1", con_slack()).await["pedir"], "perfil");
    assert_eq!(pedir(&e, "s1", con_perfil("acme")).await["pedir"], "buscar");
    let rara = json!({ "results": "### Result 1 of 1\nCanal: #equipo\nDe: Ana\nTexto: hola a todos\n", "pagination_info": "" });
    assert_eq!(pedir(&e, "s1", con_pagina("acme", rara.to_string())).await, Value::Null);
    let lectura = ultima_lectura(&e.pool, "acme").await.unwrap();
    assert!(lectura["resumen"].as_str().unwrap().contains("contestó con otra forma que la esperada"), "{lectura}");
    assert_eq!(guardados(&e.pool).await.len(), 0);

    // Un perfil que no dice quién es el usuario tampoco sirve.
    envejecer(&e.pool, "acme", 25).await;
    sqlx::query("delete from ajustes where clave = 'slack:acme:usuario'").execute(&e.pool).await.unwrap();
    assert_eq!(pedir(&e, "s1", con_slack()).await["pedir"], "perfil");
    let sin_id = PasoDeFuente { espacio: Some("acme".into()), perfil: Some("Username: ana\n".into()), ..con_slack() };
    assert_eq!(pedir(&e, "s1", sin_id).await, Value::Null);
    let lectura = ultima_lectura(&e.pool, "acme").await.unwrap();
    assert!(lectura["resumen"].as_str().unwrap().contains("no dijo quién es el usuario"), "{lectura}");
}

#[tokio::test]
async fn sin_espacios_por_conexion_no_se_le_pide_nada_al_mod() {
    let base = comun::Base::nueva().await;
    // Sin Slack encendido.
    let apagado = con_fuentes(base.pool.clone(), Fuentes::default()).await;
    assert_eq!(pedir(&apagado, "s1", con_slack()).await, Value::Null);
    // Con Slack encendido, pero todos los espacios con llave: esos los lee el servicio.
    let con_llave = Espacio { nombre: "acme".into(), llave: Llave::Fija("xoxp-de-mentira".into()) };
    let e = con_fuentes(base.pool.clone(), encendidas(vec![con_llave])).await;
    assert_eq!(pedir(&e, "s1", con_slack()).await, Value::Null);

    // Y al revés: un espacio que se lee por la conexión no lo lee el servicio, pero lo nombra.
    let solo_conexion = encendidas(vec![sin_llave("acme")]);
    let (bien, resumen) = fuentes::juntar(&base.pool, &solo_conexion, "Ana").await.unwrap();
    assert!(bien, "{resumen}");
    assert_eq!(
        resumen,
        "Slack: Aparte, 1 espacio por la conexión de Slack de Claude: lo trae Claude cuando hay una conversación abierta."
    );
}
