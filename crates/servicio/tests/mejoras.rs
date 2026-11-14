//! Etapa 3, lado del servicio: reportar una falla, preguntar, aceptar o
//! rechazar, las señales, los avisos para el chat y el buzón entre el
//! guardián y el mod que lanza al agente. El trabajo del guardián se imita
//! cambiando el estado a mano.

mod comun;

use chrono::Utc;
use comun::{busqueda, empezar, estado, nota};
use memoria::error::Error;
use memoria::estado::Estado;
use memoria::mcp::{self, PedidoAgente, PedidoEstado, PedidoMejorar, PedidoRechazar, PedidoReportar};
use memoria::modelo::TipoNota;
use serde_json::Value;
use sqlx::SqlitePool;

fn reporte(buscado: &str, sesion: &str) -> PedidoReportar {
    PedidoReportar {
        resumen: format!("La búsqueda de «{buscado}» no encuentra la nota que lo responde."),
        detalle: "Busqué eso y esperaba la nota del acuerdo con el cliente; no apareció nada.".into(),
        sesion: Some(sesion.into()),
        buscado: Some(buscado.into()),
        esperado: Some("la nota del acuerdo".into()),
        obtenido: Some("nada".into()),
        ..PedidoReportar::default()
    }
}

async fn aceptar(e: &Estado, id: i64, sesion: &str) -> Result<String, Error> {
    mcp::mejorar(e, &PedidoMejorar { id, sesion: Some(sesion.into()) }).await
}

async fn estado_de(pool: &SqlitePool, id: i64) -> String {
    sqlx::query_scalar("select estado from mejoras where id = ?1").bind(id).fetch_one(pool).await.unwrap()
}

/// Lo que ve el mod de una conversación: ids en curso y avisos pendientes.
async fn avisos(e: &Estado, sesion: &str, vistos: &[i64]) -> (Vec<i64>, Vec<Value>) {
    let pedido = PedidoEstado { sesion: Some(sesion.into()), vistos: vistos.to_vec(), ..PedidoEstado::default() };
    let v: Value = serde_json::from_str(&mcp::mejora_estado(e, &pedido).await.unwrap()).unwrap();
    let en_curso = v["en_curso"].as_array().unwrap().iter().map(|i| i.as_i64().unwrap()).collect();
    (en_curso, v["avisos"].as_array().unwrap().clone())
}

fn ids(lista: &Value) -> Vec<i64> {
    lista.as_array().unwrap().iter().map(|i| i.as_i64().unwrap_or_else(|| i["id"].as_i64().unwrap())).collect()
}

/// Lo que ve el mod sobre agentes: qué puede lanzar esta conversación y qué
/// agentes suyos siguen anotados. `agentes` son los que avisa que siguen andando.
async fn agentes(e: &Estado, sesion: &str, andando: &[i64]) -> (Vec<i64>, Vec<i64>) {
    let pedido = PedidoEstado { sesion: Some(sesion.into()), agentes: andando.to_vec(), ..PedidoEstado::default() };
    let v: Value = serde_json::from_str(&mcp::mejora_estado(e, &pedido).await.unwrap()).unwrap();
    (ids(&v["para_agente"]), ids(&v["con_agente"]))
}

/// Un paso del agente, como lo anota el mod. Devuelve si valió.
async fn paso(e: &Estado, id: i64, sesion: &str, paso: &str, texto: Option<&str>) -> bool {
    let pedido = PedidoAgente { id, sesion: sesion.into(), paso: paso.into(), texto: texto.map(Into::into) };
    let v: Value = serde_json::from_str(&mcp::agente(e, &pedido).await.unwrap()).unwrap();
    v["ok"].as_bool().unwrap()
}

/// El guardián tomó la mejora, armó la copia y dejó el encargo.
async fn dejar_lista(pool: &SqlitePool, id: i64, hace_horas: i64) {
    let encargo = serde_json::json!({
        "copia": format!("/tmp/memoria/trabajo/mejora-{id}"),
        "cargo": format!("/tmp/memoria/trabajo/mejora-{id}-cargo"),
        "pedido": "Arreglá la búsqueda.",
    });
    sqlx::query(
        "update mejoras set estado = 'en_curso', fase = 'esperando_agente', fase_desde = ?2, empezada = ?3,
                agente_sesion = null, agente_visto = null, encargo = ?4
         where id = ?1",
    )
    .bind(id)
    .bind(Utc::now())
    .bind(Utc::now() - chrono::Duration::hours(hace_horas))
    .bind(encargo.to_string())
    .execute(pool)
    .await
    .unwrap();
}

async fn texto_de(pool: &SqlitePool, id: i64, columna: &str) -> Option<String> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select {columna} from mejoras where id = ?1")))
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn una_falla_se_reporta_se_pregunta_y_se_acepta() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    mcp::guardar(
        &e,
        nota(TipoNota::Acuerdo, "Entregas los viernes", "Acordado con el cliente: se entrega cada viernes.", "local_a"),
    )
    .await
    .unwrap();

    let mut primero = reporte("cuándo son las entregas del viernes", "local_a");
    primero.notas_esperadas = vec![1];
    primero.toca_base = true;
    primero.suma_librerias = true;
    let r = mcp::reportar(&e, &primero).await.unwrap();
    assert!(r.starts_with("Quedó registrada como mejora 1. Preguntale al usuario"), "{r}");
    assert!(r.contains("puede necesitar cambiar la estructura de la base, sumar librerías"), "{r}");
    assert!(r.contains("memoria_mejorar con id 1") && r.contains("memoria_rechazar"), "{r}");
    let (origen, conversacion, grupo, ejemplo, toca_base): (String, Option<String>, Option<String>, Value, bool) =
        sqlx::query_as(
            "select m.origen, m.conversacion, g.proyecto, m.ejemplo, m.toca_base
             from mejoras m left join grupos g on g.id = m.grupo_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        (origen.as_str(), conversacion.as_deref(), grupo.as_deref()),
        ("claude", Some("local_a"), Some("Portal"))
    );
    assert_eq!(ejemplo["buscado"], "cuándo son las entregas del viernes");
    assert_eq!(ejemplo["notas_esperadas"], serde_json::json!([1]));
    assert!(toca_base);

    // El mismo problema dicho de otra forma no se registra dos veces.
    let r = mcp::reportar(&e, &reporte("entregas viernes cuando", "local_a")).await.unwrap();
    assert!(r.starts_with("Esto ya estaba registrado como mejora 1 y el usuario todavía no contestó"), "{r}");
    let r = mcp::mejora_estado(&e, &PedidoEstado { id: Some(1), ..PedidoEstado::default() }).await.unwrap();
    assert_eq!(r, "Mejora 1: registrada; falta que el usuario diga si la quiere.");

    // El usuario dice que sí.
    let r = aceptar(&e, 1, "local_a").await.unwrap();
    assert!(r.starts_with("Mejora 1 aceptada. Un agente la va a trabajar de fondo"), "{r}");
    assert_eq!(estado_de(&pool, 1).await, "aceptada");
    let r = mcp::reportar(&e, &reporte("cuándo son las entregas del viernes", "local_a")).await.unwrap();
    assert!(r.starts_with("Ya hay una mejora en curso para esto (mejora 1)"), "{r}");
    let Err(Error::Uso(m)) = aceptar(&e, 1, "local_a").await else { panic!("ya estaba aceptada") };
    assert!(m.contains("ya está aceptada"), "{m}");
    let Err(Error::Uso(m)) = mcp::rechazar(&e, &PedidoRechazar { id: 1, motivo: None }).await else {
        panic!("ya no se puede rechazar")
    };
    assert!(m.contains("no se puede rechazar ahora"), "{m}");
    let Err(Error::Uso(m)) = aceptar(&e, 99, "local_a").await else { panic!("no existe") };
    assert_eq!(m, "No existe la mejora 99.");
}

#[tokio::test]
async fn si_el_usuario_dice_que_no_no_se_vuelve_a_preguntar() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    mcp::reportar(&e, &reporte("región de los deploys", "local_a")).await.unwrap();
    let r = mcp::rechazar(&e, &PedidoRechazar { id: 1, motivo: Some("no vale la pena".into()) }).await.unwrap();
    assert_eq!(r, "Mejora 1 rechazada: no se va a volver a preguntar por lo mismo.");
    let r = mcp::reportar(&e, &reporte("la región de los deploys", "local_a")).await.unwrap();
    assert_eq!(r, "El usuario ya dijo que no a esta mejora (mejora 1): no se la vuelvas a preguntar.");
    // Otro ejemplo es otro problema.
    let r = mcp::reportar(&e, &reporte("paginación de los listados", "local_a")).await.unwrap();
    assert!(r.starts_with("Quedó registrada como mejora 2"), "{r}");
    // Si el usuario cambia de idea, se puede retomar la rechazada.
    aceptar(&e, 1, "local_a").await.unwrap();
    assert_eq!(estado_de(&pool, 1).await, "aceptada");
}

#[tokio::test]
async fn no_registra_reportes_incompletos_ni_con_secretos() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    let mut con_token = reporte("token del bot", "local_a");
    con_token.detalle = "Apareció ghp_abcdefghijklmnopqrstuvwxyz0123456789 en el resultado.".into();
    let mut inventada = reporte("algo", "local_a");
    inventada.notas_esperadas = vec![99];
    let mut vacio = reporte("algo", "local_a");
    vacio.resumen = "  ".into();
    for (pedido, esperado) in [
        (con_token, "parece tener un token de GitHub"),
        (inventada, "No existe la nota 99"),
        (vacio, "necesita un resumen"),
    ] {
        let Err(Error::Uso(m)) = mcp::reportar(&e, &pedido).await else { panic!("debía rechazarlo: {esperado}") };
        assert!(m.contains(esperado), "{m}");
    }
    let cuantas: i64 = sqlx::query_scalar("select count(*) from mejoras").fetch_one(&pool).await.unwrap();
    assert_eq!(cuantas, 0);
    // Sin conversación ni grupo también se puede reportar.
    let suelto = PedidoReportar { sesion: None, ..reporte("otra cosa", "x") };
    assert!(mcp::reportar(&e, &suelto).await.unwrap().starts_with("Quedó registrada como mejora 1"));
}

#[tokio::test]
async fn tres_busquedas_vacias_parecidas_proponen_una_mejora() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_b", "ACME", Some("Portal")).await;
    // La misma consulta repetida en un solo chat no alcanza.
    for _ in 0..4 {
        let r = mcp::buscar(&e, &busqueda("receta de tarta", "local_a")).await.unwrap();
        assert!(r.starts_with("Nada para") && !r.contains("mejora"), "{r}");
    }
    let r = mcp::buscar(&e, &busqueda("facturación de octubre", "local_a")).await.unwrap();
    assert!(!r.contains("mejora"), "{r}");
    let r = mcp::buscar(&e, &busqueda("facturación del mes de octubre", "local_b")).await.unwrap();
    assert!(!r.contains("mejora"), "{r}");
    // La tercera parecida, en el mismo grupo, propone la mejora.
    let r = mcp::buscar(&e, &busqueda("facturación octubre Globex", "local_a")).await.unwrap();
    assert!(r.starts_with("Nada para «facturación octubre Globex»"), "{r}");
    assert!(r.contains("Quedó registrada como mejora 1") && r.contains("memoria_mejorar con id 1"), "{r}");
    let (origen, estado, resumen, conversacion, buscado): (String, String, String, Option<String>, String) =
        sqlx::query_as("select origen, estado, resumen, conversacion, ejemplo->>'buscado' from mejoras")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((origen.as_str(), estado.as_str(), conversacion.as_deref()), ("servicio", "propuesta", Some("local_a")));
    assert_eq!(buscado, "facturación octubre Globex");
    assert!(
        resumen.starts_with("En ACME › Portal se buscó 3 veces algo parecido a «facturación octubre Globex»"),
        "{resumen}"
    );
    // Mientras siga propuesta no se insiste ni se duplica.
    let r = mcp::buscar(&e, &busqueda("facturación de octubre", "local_b")).await.unwrap();
    assert!(!r.contains("mejora"), "{r}");
    let cuantas: i64 = sqlx::query_scalar("select count(*) from mejoras").fetch_one(&pool).await.unwrap();
    assert_eq!(cuantas, 1);

    // Ya se trabajó: las búsquedas vacías de antes no cuentan, hacen falta tres nuevas.
    sqlx::query("update mejoras set estado = 'instalada', terminada = ?1 where id = 1")
        .bind(Utc::now())
        .execute(&pool)
        .await
        .unwrap();
    for (consulta, sesion) in [("facturación de octubre", "local_a"), ("facturación del mes de octubre", "local_b")] {
        let r = mcp::buscar(&e, &busqueda(consulta, sesion)).await.unwrap();
        assert!(!r.contains("mejora"), "{r}");
    }
    let r = mcp::buscar(&e, &busqueda("facturación octubre Globex", "local_a")).await.unwrap();
    assert!(r.contains("Quedó registrada como mejora 2"), "{r}");
}

#[tokio::test]
async fn cada_busqueda_deja_su_senal() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    mcp::guardar(&e, nota(TipoNota::Dato, "Región de los deploys", "Los deploys van a us-east-1.", "local_a"))
        .await
        .unwrap();
    mcp::buscar(&e, &busqueda("región deploys", "local_a")).await.unwrap();
    let (tipo, conversacion, detalle): (String, Option<String>, Value) =
        sqlx::query_as("select tipo, conversacion, detalle from senales").fetch_one(&pool).await.unwrap();
    assert_eq!((tipo.as_str(), conversacion.as_deref()), ("busqueda", Some("local_a")));
    assert_eq!(detalle["consulta"], "región deploys");
    assert_eq!(detalle["grupo"], "ACME › Portal");
    assert_eq!(detalle["resultados"], serde_json::json!(["nota:1"]));
    assert!(detalle["ms"].is_u64(), "{detalle}");
}

#[tokio::test]
async fn los_avisos_llegan_al_chat_donde_se_acepto() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_b", "ACME", Some("Portal")).await;
    empezar(&e, "local_f", "ACME", Some("Tienda")).await;
    mcp::reportar(&e, &reporte("entregas del viernes", "local_b")).await.unwrap();
    // Se reportó en un chat y el usuario la acepta en otro: el aviso va donde aceptó.
    aceptar(&e, 1, "local_a").await.unwrap();
    assert_eq!(avisos(&e, "local_a", &[]).await, (vec![1], vec![]));
    assert_eq!(avisos(&e, "local_b", &[]).await, (vec![], vec![]));

    // El guardián la toma y la termina.
    sqlx::query("update mejoras set estado = 'en_curso', empezada = ?1 where id = 1")
        .bind(Utc::now())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(avisos(&e, "local_a", &[]).await.0, vec![1]);
    let r = mcp::mejora_estado(&e, &PedidoEstado { id: Some(1), ..PedidoEstado::default() }).await.unwrap();
    assert_eq!(r, "Mejora 1: en curso desde hace 0 min.");
    sqlx::query(
        "update mejoras set estado = 'instalada', cambios = 'Ahora la búsqueda reconoce sinónimos.', terminada = ?1
         where id = 1",
    )
    .bind(Utc::now())
    .execute(&pool)
    .await
    .unwrap();
    let (en_curso, pendientes) = avisos(&e, "local_a", &[]).await;
    assert_eq!(en_curso, Vec::<i64>::new());
    assert_eq!(pendientes.len(), 1);
    assert_eq!(pendientes[0]["id"], 1);
    assert_eq!(pendientes[0]["instalada"], true);
    assert_eq!(pendientes[0]["texto"], "Memoria: mejora 1 instalada. Ahora la búsqueda reconoce sinónimos.");
    assert_eq!(pendientes[0]["buscado"], "entregas del viernes");
    // Mientras el mod no confirme que lo mostró, sigue pendiente; otros chats no lo ven todavía.
    assert_eq!(avisos(&e, "local_a", &[]).await.1.len(), 1);
    assert_eq!(avisos(&e, "local_b", &[]).await.1, Vec::<Value>::new());

    // Si ese chat no vuelve a abrirse, pasadas unas horas aparece en otro del mismo grupo.
    sqlx::query("update mejoras set terminada = ?1 where id = 1")
        .bind(Utc::now() - chrono::Duration::hours(7))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(avisos(&e, "local_b", &[]).await.1.len(), 1);
    assert_eq!(avisos(&e, "local_f", &[]).await.1, Vec::<Value>::new(), "otro grupo no lo ve");
    // El mod confirma que lo mostró: no aparece más en ningún lado.
    assert_eq!(avisos(&e, "local_b", &[1]).await.1, Vec::<Value>::new());
    assert_eq!(avisos(&e, "local_a", &[]).await.1, Vec::<Value>::new());

    // Una que no se pudo: el aviso dice que todo quedó como estaba, y por qué.
    mcp::reportar(&e, &reporte("paginación de los listados", "local_a")).await.unwrap();
    aceptar(&e, 2, "local_a").await.unwrap();
    sqlx::query(
        "update mejoras set estado = 'fallida', motivo = 'El cambio rompía dos preguntas de la batería.', terminada = ?1
         where id = 2",
    )
    .bind(Utc::now())
    .execute(&pool)
    .await
    .unwrap();
    let pendientes = avisos(&e, "local_a", &[]).await.1;
    assert_eq!(pendientes[0]["instalada"], false);
    assert_eq!(
        pendientes[0]["texto"],
        "Memoria: no pude hacer la mejora 2; quedó todo como estaba. El cambio rompía dos preguntas de la batería."
    );
    // Una que falló se puede volver a intentar.
    aceptar(&e, 2, "local_a").await.unwrap();
    assert_eq!(estado_de(&pool, 2).await, "aceptada");
    assert_eq!(avisos(&e, "local_a", &[]).await.1, Vec::<Value>::new());
}

#[tokio::test]
async fn el_agente_lo_lanza_la_conversacion_donde_se_acepto() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_b", "ACME", Some("Portal")).await;
    empezar(&e, "local_f", "ACME", Some("Tienda")).await;
    mcp::reportar(&e, &reporte("entregas del viernes", "local_a")).await.unwrap();
    let dicho = aceptar(&e, 1, "local_a").await.unwrap();
    assert!(dicho.contains("dentro de esta misma conversación"), "{dicho}");
    // Hasta que el guardián no deja la copia lista, no hay nada que lanzar.
    assert_eq!(agentes(&e, "local_a", &[]).await, (vec![], vec![]));
    assert!(!paso(&e, 1, "local_a", "empezo", None).await);

    dejar_lista(&pool, 1, 0).await;
    let pedido = PedidoEstado { sesion: Some("local_a".into()), ..PedidoEstado::default() };
    let v: Value = serde_json::from_str(&mcp::mejora_estado(&e, &pedido).await.unwrap()).unwrap();
    // El encargo del guardián llega tal cual.
    assert_eq!(v["para_agente"][0]["id"], 1);
    assert_eq!(v["para_agente"][0]["encargo"]["copia"], "/tmp/memoria/trabajo/mejora-1");
    assert_eq!(v["para_agente"][0]["encargo"]["pedido"], "Arreglá la búsqueda.");
    assert_eq!(v["en_curso"], serde_json::json!([1]));
    let cuenta = |id| PedidoEstado { id: Some(id), ..PedidoEstado::default() };
    assert!(mcp::mejora_estado(&e, &cuenta(1)).await.unwrap().starts_with("Mejora 1: lista para el agente."));
    // Otra conversación, aunque sea del mismo grupo, no la ve ni la puede tomar.
    assert_eq!(agentes(&e, "local_b", &[]).await, (vec![], vec![]));
    assert!(!paso(&e, 1, "local_b", "empezo", None).await);

    // La toma la conversación donde se aceptó, una sola vez.
    assert!(paso(&e, 1, "local_a", "empezo", None).await);
    assert!(!paso(&e, 1, "local_a", "empezo", None).await);
    assert_eq!(agentes(&e, "local_a", &[]).await, (vec![], vec![1]));
    assert_eq!(texto_de(&pool, 1, "agente_sesion").await.as_deref(), Some("local_a"));
    assert_eq!(texto_de(&pool, 1, "cast(lanzamientos as text)").await.as_deref(), Some("1"));
    assert_eq!(
        mcp::mejora_estado(&e, &cuenta(1)).await.unwrap(),
        "Mejora 1: un agente la trabaja de fondo en una conversación de Claude desde hace 0 min."
    );

    // El mod avisa que el agente sigue andando: solo vale el de la conversación que lo lanzó.
    let viejo = Utc::now() - chrono::Duration::minutes(5);
    sqlx::query("update mejoras set agente_visto = ?1 where id = 1").bind(viejo).execute(&pool).await.unwrap();
    let visto = || async { texto_de(&pool, 1, "agente_visto").await.unwrap() };
    let antes = visto().await;
    agentes(&e, "local_b", &[1]).await;
    assert_eq!(visto().await, antes);
    agentes(&e, "local_a", &[1]).await;
    assert!(visto().await > antes, "el aviso de la conversación del agente renueva la marca");

    // Solo esa conversación puede decir cómo terminó.
    assert!(!paso(&e, 1, "local_b", "termino", Some("Listo.")).await);
    assert!(paso(&e, 1, "local_a", "fallo", Some("  Se cortó la conversación.  ")).await);
    assert_eq!(texto_de(&pool, 1, "fase").await.as_deref(), Some("agente_fallo"));
    assert_eq!(texto_de(&pool, 1, "motivo").await.as_deref(), Some("Se cortó la conversación."));
    assert_eq!(agentes(&e, "local_a", &[]).await, (vec![], vec![]));
    assert!(mcp::mejora_estado(&e, &cuenta(1)).await.unwrap().contains("se va a lanzar de nuevo"));

    // El guardián la deja lista otra vez; el segundo agente termina.
    dejar_lista(&pool, 1, 0).await;
    assert!(paso(&e, 1, "local_a", "empezo", None).await);
    assert_eq!(texto_de(&pool, 1, "cast(lanzamientos as text)").await.as_deref(), Some("2"));
    assert!(paso(&e, 1, "local_a", "termino", Some(" Bajé el piso de parecido de las notas. ")).await);
    assert!(!paso(&e, 1, "local_a", "termino", Some("De nuevo.")).await);
    assert_eq!(texto_de(&pool, 1, "fase").await.as_deref(), Some("agente_listo"));
    assert_eq!(texto_de(&pool, 1, "informe").await.as_deref(), Some("Bajé el piso de parecido de las notas."));
    assert!(mcp::mejora_estado(&e, &cuenta(1)).await.unwrap().contains("el agente terminó"));
    assert_eq!(agentes(&e, "local_a", &[]).await, (vec![], vec![]));

    // Un paso que no existe, o de una conversación desconocida, es un error de uso.
    for (sesion, que) in [("local_a", "otro"), ("local_nadie", "empezo")] {
        let pedido = PedidoAgente { id: 1, sesion: sesion.into(), paso: que.into(), texto: None };
        assert!(matches!(mcp::agente(&e, &pedido).await, Err(Error::Uso(_))), "{sesion} {que}");
    }

    // Si no se pudo y el usuario la acepta de nuevo, arranca limpia.
    sqlx::query("update mejoras set estado = 'fallida', fase = null, terminada = ?1 where id = 1")
        .bind(Utc::now())
        .execute(&pool)
        .await
        .unwrap();
    aceptar(&e, 1, "local_a").await.unwrap();
    for columna in ["fase", "encargo", "informe", "agente_sesion", "agente_visto"] {
        assert_eq!(texto_de(&pool, 1, columna).await, None, "{columna}");
    }
    assert_eq!(texto_de(&pool, 1, "cast(lanzamientos as text)").await.as_deref(), Some("0"));
}

#[tokio::test]
async fn una_mejora_sin_su_conversacion_la_toma_otra_del_grupo() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_b", "ACME", Some("Portal")).await;
    empezar(&e, "local_f", "ACME", Some("Tienda")).await;
    for buscado in ["entregas del viernes", "paginación de los listados", "permisos de los operadores"] {
        mcp::reportar(&e, &reporte(buscado, "local_a")).await.unwrap();
    }
    for id in 1..=3 {
        aceptar(&e, id, "local_a").await.unwrap();
    }

    // La conversación donde se aceptó no volvió a abrirse en horas: la puede
    // tomar otra del mismo grupo, y ninguna de otro.
    dejar_lista(&pool, 1, 7).await;
    assert_eq!(agentes(&e, "local_b", &[]).await.0, vec![1]);
    assert_eq!(agentes(&e, "local_f", &[]).await.0, Vec::<i64>::new());
    assert!(!paso(&e, 1, "local_f", "empezo", None).await);
    assert!(paso(&e, 1, "local_b", "empezo", None).await);
    assert_eq!(agentes(&e, "local_b", &[]).await, (vec![], vec![1]));
    // Sigue apareciendo en curso donde se aceptó, que es donde va el aviso.
    assert_eq!(avisos(&e, "local_a", &[]).await.0, vec![1, 2, 3]);

    // Una que no quedó atada a ninguna conversación la toma cualquiera.
    dejar_lista(&pool, 2, 0).await;
    sqlx::query("update mejoras set conversacion = null where id = 2").execute(&pool).await.unwrap();
    assert_eq!(agentes(&e, "local_f", &[]).await.0, vec![2]);
    assert!(paso(&e, 2, "local_f", "empezo", None).await);

    // Con dos agentes trabajando, la tercera espera su turno.
    dejar_lista(&pool, 3, 0).await;
    assert_eq!(agentes(&e, "local_a", &[]).await.0, Vec::<i64>::new());
    assert!(!paso(&e, 3, "local_a", "empezo", None).await);
    assert!(paso(&e, 2, "local_f", "termino", Some("Listo.")).await);
    assert_eq!(agentes(&e, "local_a", &[]).await.0, vec![3]);
    assert!(paso(&e, 3, "local_a", "empezo", None).await);
}
