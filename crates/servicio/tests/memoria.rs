//! Pruebas contra una base de verdad: cada una corre sobre un archivo nuevo
//! con las migraciones de `migraciones/`.

mod comun;

use comun::{busqueda, empezar, estado, nota};
use memoria::error::Error;
use memoria::grupos::Registro;
use memoria::mcp::{self, PedidoBuscar, PedidoContexto};
use memoria::modelo::TipoNota;

#[tokio::test]
async fn una_decision_de_portal_aparece_en_otro_chat_de_portal() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let inicio = empezar(&e, "local_a", "ACME", Some("Portal")).await;
    assert!(inicio.contains("Grupo: ACME › Portal"), "{inicio}");
    assert!(inicio.contains("Todavía no hay notas"), "{inicio}");

    let r = mcp::guardar(
        &e,
        nota(
            TipoNota::Decision,
            "Usar Cognito para la sesión de Portal",
            "Qué: la sesión de Portal pasa a Cognito. Por qué: Auth0 cobraba por usuario. Cómo: pool en infra/auth.ts.",
            "local_a",
        ),
    )
    .await
    .unwrap();
    assert_eq!(r, "Guardada como nota 1 en ACME › Portal (decisión).");

    // Otro chat del mismo grupo: el resumen ya la trae y la búsqueda la
    // encuentra aunque se escriba sin acentos.
    let otro = empezar(&e, "local_b", "acme", Some("PORTAL")).await;
    assert!(otro.contains("Decisiones vigentes:"), "{otro}");
    assert!(otro.contains("[nota 1]"), "{otro}");
    let hallado = mcp::buscar(&e, &busqueda("sesion cognito", "local_b")).await.unwrap();
    assert!(hallado.starts_with("1 resultado para «sesion cognito» en ACME › Portal"), "{hallado}");
    assert!(hallado.contains("[nota 1] decisión · vigente"), "{hallado}");
    assert!(hallado.contains("origen local_a"), "{hallado}");
    assert!(!hallado.contains("· de "), "es del mismo grupo: {hallado}");

    let grupos: i64 = sqlx::query_scalar("select count(*) from grupos").fetch_one(&pool).await.unwrap();
    assert_eq!(grupos, 1, "«acme › PORTAL» es el mismo grupo que «ACME › Portal»");
}

#[tokio::test]
async fn amplia_a_la_empresa_y_a_todo_si_hay_pocos() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_tienda", "ACME", Some("Tienda")).await;
    empezar(&e, "local_globex", "Globex", None).await;
    empezar(&e, "local_portal", "ACME", Some("Portal")).await;
    mcp::guardar(
        &e,
        nota(
            TipoNota::Error,
            "Deploy falla por la región",
            "Error: el deploy usa us-east-2. Causa: perfil viejo. Solución: AWS_REGION fija.",
            "local_tienda",
        ),
    )
    .await
    .unwrap();
    mcp::guardar(
        &e,
        nota(TipoNota::Dato, "Región por defecto de los deploys", "Los deploys van a us-east-1.", "local_globex"),
    )
    .await
    .unwrap();

    let r = mcp::buscar(&e, &busqueda("region deploy", "local_portal")).await.unwrap();
    assert!(r.contains("amplié a ACME y a todo"), "{r}");
    assert!(r.contains("· de ACME › Tienda"), "{r}");
    assert!(r.contains("· de Globex"), "{r}");
    // Lo de la empresa aparece antes que lo de afuera.
    assert!(r.find("ACME › Tienda").unwrap() < r.find("de Globex").unwrap(), "{r}");
}

#[tokio::test]
async fn guardar_dos_veces_lo_mismo_suma_el_origen() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_b", "ACME", Some("Portal")).await;
    let n = |s: &str| {
        nota(TipoNota::Acuerdo, "Entregas los viernes", "Acordado con el cliente: se entrega cada viernes.", s)
    };
    mcp::guardar(&e, n("local_a")).await.unwrap();
    let r = mcp::guardar(&e, n("local_b")).await.unwrap();
    assert!(r.starts_with("Ya estaba guardada igual como nota 1"), "{r}");

    let (cuantas, origenes): (i64, String) =
        sqlx::query_as("select count(*) over (), origenes from notas").fetch_one(&pool).await.unwrap();
    assert_eq!(cuantas, 1);
    let mut origenes = memoria::base::lista(&origenes);
    origenes.sort();
    assert_eq!(origenes, vec!["local_a", "local_b"]);
}

#[tokio::test]
async fn una_decision_nueva_reemplaza_la_vieja() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    mcp::guardar(&e, nota(TipoNota::Decision, "Login con Auth0", "Qué: el login usa Auth0.", "local_a")).await.unwrap();
    let mut nueva =
        nota(TipoNota::Decision, "Login con Cognito", "Qué: el login pasa a Cognito. Por qué: costo.", "local_a");
    nueva.reemplaza = Some(1);
    let r = mcp::guardar(&e, nueva).await.unwrap();
    assert_eq!(r, "Guardada como nota 2 en ACME › Portal (decisión); la nota 1 quedó como reemplazada.");

    // La búsqueda muestra la que vale y nombra debajo la que se usaba antes.
    let b = mcp::buscar(&e, &busqueda("login", "local_a")).await.unwrap();
    assert!(b.starts_with("1 resultado para «login»"), "{b}");
    assert!(b.contains("[nota 2] decisión · vigente"), "{b}");
    assert!(b.contains("Antes se usaba: [nota 1] Login con Auth0 (reemplazada)"), "{b}");
    // Aunque solo coincida la vieja, lleva a la que vale.
    let b = mcp::buscar(&e, &busqueda("auth0", "local_a")).await.unwrap();
    assert!(b.contains("[nota 2] decisión · vigente") && b.contains("— Login con Cognito"), "{b}");
    assert!(b.contains("Antes se usaba: [nota 1] Login con Auth0 (reemplazada)"), "{b}");
    // Si se piden, las reemplazadas van como resultados propios, al final.
    let con_viejas = PedidoBuscar { reemplazadas: true, ..busqueda("login", "local_a") };
    let b = mcp::buscar(&e, &con_viejas).await.unwrap();
    let vigente = b.find("[nota 2] decisión · vigente").expect(&b);
    let vieja = b.find("[nota 1] decisión · reemplazada por la nota 2").expect(&b);
    assert!(vigente < vieja, "las vigentes van primero: {b}");
    assert!(!b.contains("Antes se usaba"), "{b}");

    let resumen = empezar(&e, "local_a", "ACME", Some("Portal")).await;
    assert!(resumen.contains("Login con Cognito"), "{resumen}");
    assert!(!resumen.contains("Login con Auth0"), "el resumen solo trae vigentes: {resumen}");

    let mut otra = nota(TipoNota::Decision, "Login con Okta", "Qué: Okta.", "local_a");
    otra.reemplaza = Some(1);
    let Err(Error::Uso(m)) = mcp::guardar(&e, otra).await else { panic!("debía rechazarla") };
    assert!(m.contains("ya está reemplazada"), "{m}");
}

#[tokio::test]
async fn no_guarda_claves_ni_tokens() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    let con_token =
        nota(TipoNota::Dato, "Token del bot", "El token es ghp_abcdefghijklmnopqrstuvwxyz0123456789", "local_a");
    let Err(Error::Uso(m)) = mcp::guardar(&e, con_token).await else { panic!("debía rechazarla") };
    assert!(m.contains("un token de GitHub"), "{m}");
    let cuantas: i64 = sqlx::query_scalar("select count(*) from notas").fetch_one(&pool).await.unwrap();
    assert_eq!(cuantas, 0);
}

#[tokio::test]
async fn sin_grupo_lo_pide_y_con_grupo_lo_registra() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let mut suelta = nota(TipoNota::Dato, "Algo", "Algo útil.", "local_x");
    suelta.sesion = None;
    let Err(Error::Uso(m)) = mcp::guardar(&e, suelta).await else { panic!("debía pedir grupo") };
    assert!(m.starts_with("Falta el grupo"), "{m}");

    let desconocida = PedidoContexto { sesion: "local_x".into(), grupo: None, registro: None };
    let Err(Error::Uso(m)) = mcp::contexto(&e, &desconocida).await else { panic!("debía pedir grupo") };
    assert!(m.contains("No conozco la conversación «local_x»"), "{m}");

    // Desde la terminal, sin mod: Claude pasa el grupo a mano y queda registrada.
    let con_grupo = PedidoContexto { sesion: "local_x".into(), grupo: Some("ACME > Agenda".into()), registro: None };
    let r = mcp::contexto(&e, &con_grupo).await.unwrap();
    assert!(r.starts_with("Grupo: ACME › Agenda"), "{r}");
    // Escrito después con otras mayúsculas es el mismo grupo, con el nombre de la primera vez.
    let otra_vez = PedidoContexto { sesion: "local_z".into(), grupo: Some("acme/AGENDA".into()), registro: None };
    let r = mcp::contexto(&e, &otra_vez).await.unwrap();
    assert!(r.starts_with("Grupo: ACME › Agenda"), "{r}");
    let r = mcp::guardar(&e, nota(TipoNota::Pendiente, "Revisar alarmas", "Pendiente: revisar alarmas.", "local_x"))
        .await
        .unwrap();
    assert_eq!(r, "Guardada como nota 1 en ACME › Agenda (pendiente).");

    // Un grupo escrito como en el costado de la app también sirve.
    empezar(&e, "local_y", "ACME", Some("Portal")).await;
    let mut a_portal = nota(TipoNota::Dato, "Dato de Portal", "Un dato.", "local_x");
    a_portal.grupo = Some("[ACME] - PORTAL".into());
    let r = mcp::guardar(&e, a_portal).await.unwrap();
    assert!(r.ends_with("en ACME › Portal (dato)."), "{r}");

    let mut inexistente = busqueda("alarmas", "local_x");
    inexistente.grupo = Some("Umbrella".into());
    let Err(Error::Uso(m)) = mcp::buscar(&e, &inexistente).await else { panic!("debía avisar") };
    assert!(m.contains("No conozco el grupo «Umbrella»") && m.contains("ACME › Agenda"), "{m}");
}

#[tokio::test]
async fn el_grupo_sale_del_costado_de_la_app_o_de_la_carpeta() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    // Lo que manda el mod: el nombre del grupo del costado y la carpeta. Devuelve el primer renglón del resumen.
    let registrar = |sesion: &'static str, grupo_app: Option<&'static str>, carpeta: Option<&'static str>| {
        let e = e.clone();
        async move {
            let registro = Registro {
                grupo_app: grupo_app.map(Into::into),
                carpeta: carpeta.map(Into::into),
                ..Registro::default()
            };
            let pedido = PedidoContexto { sesion: sesion.into(), grupo: None, registro: Some(registro) };
            mcp::contexto(&e, &pedido).await.unwrap().lines().next().unwrap_or_default().to_owned()
        }
    };
    let portal = "/Users/ana/proyectos/acme/portal";
    // El nombre del costado manda: «[Empresa] - Proyecto».
    assert_eq!(
        registrar("local_a", Some("[ACME] - Portal"), Some(portal)).await,
        "Grupo: ACME › Portal · 0 notas vigentes"
    );
    // Con otras mayúsculas es el mismo grupo, y queda el nombre de la primera vez.
    assert!(registrar("local_b", Some("[acme] - PORTAL"), None).await.starts_with("Grupo: ACME › Portal ·"));
    // «[Empresa]» sola es la empresa en sí; un nombre sin corchetes va en «Otras».
    assert!(registrar("local_c", Some("[GLOBEX]"), None).await.starts_with("Grupo: GLOBEX ·"));
    assert!(registrar("local_d", Some("Cosas de casa"), None).await.starts_with("Grupo: Otras › Cosas de casa ·"));

    // Sin grupo del costado: el grupo donde se trabajó la última vez en esa carpeta…
    assert!(registrar("local_e", None, Some(portal)).await.starts_with("Grupo: ACME › Portal ·"));
    // …o en la que la contiene.
    let adentro = "/Users/ana/proyectos/acme/portal/web/src";
    assert!(registrar("local_f", None, Some(adentro)).await.starts_with("Grupo: ACME › Portal ·"));
    // Una carpeta que nunca se vio va con su nombre, y sin carpeta, a «Sin grupo».
    let recetas = "/Users/ana/proyectos/recetas";
    assert!(registrar("local_g", None, Some(recetas)).await.starts_with("Grupo: Otras › recetas ·"));
    assert!(registrar("local_h", None, None).await.starts_with("Grupo: Otras › Sin grupo ·"));
    // La carpeta personal no dice nada: no junta todo lo que cuelga de ella.
    assert!(registrar("local_i", None, Some("/Users/ana")).await.starts_with("Grupo: Otras › ana ·"));
    assert!(registrar("local_j", None, Some("/Users/ana/otra")).await.starts_with("Grupo: Otras › otra ·"));

    // Una conversación ya registrada que llega sin grupo del costado (la app
    // todavía no respondió) se queda donde estaba.
    assert!(registrar("local_c", None, Some(recetas)).await.starts_with("Grupo: GLOBEX ·"));
    // Si pasa a otro grupo del costado, se muda; y la carpeta aprende lo último.
    assert!(registrar("local_c", Some("[ACME] - Tienda"), Some(recetas)).await.starts_with("Grupo: ACME › Tienda ·"));
    assert!(registrar("local_k", None, Some(recetas)).await.starts_with("Grupo: ACME › Tienda ·"));

    let grupos: Vec<(String, String)> =
        sqlx::query_as("select empresa, proyecto from grupos order by empresa, proyecto")
            .fetch_all(&pool)
            .await
            .unwrap();
    let nombres: Vec<String> = grupos.iter().map(|(e, p)| format!("{e}/{p}")).collect();
    assert_eq!(
        nombres,
        [
            "ACME/Portal",
            "ACME/Tienda",
            "GLOBEX/",
            "Otras/Cosas de casa",
            "Otras/Sin grupo",
            "Otras/ana",
            "Otras/otra",
            "Otras/recetas"
        ]
    );
}

#[tokio::test]
async fn una_busqueda_vacia_deja_una_senal() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    let r = mcp::buscar(&e, &busqueda("facturación de octubre", "local_a")).await.unwrap();
    assert!(r.starts_with("Nada para «facturación de octubre» en ACME › Portal"), "{r}");
    let (tipo, conversacion, consulta): (String, Option<String>, String) =
        sqlx::query_as("select tipo, conversacion, detalle->>'consulta' from senales").fetch_one(&pool).await.unwrap();
    assert_eq!(tipo, "busqueda_vacia");
    assert_eq!(conversacion.as_deref(), Some("local_a"));
    assert_eq!(consulta, "facturación de octubre");

    let sin_sesion = PedidoBuscar { sesion: Some("local_nueva".into()), ..busqueda("algo", "x") };
    let r = mcp::buscar(&e, &sin_sesion).await.unwrap();
    assert!(r.starts_with("No conozco esta conversación, así que busqué en todo."), "{r}");
}

#[tokio::test]
async fn al_arrancar_arma_las_palabras_que_falten() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    // Sin huellas: lo que aparece, aparece por palabras.
    let e = comun::estado_sin_huellas(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    let cuerpo = "Qué: la sesión de Portal pasa a Cognito. Por qué: Auth0 cobraba por usuario.";
    mcp::guardar(&e, nota(TipoNota::Decision, "Usar Cognito para la sesión", cuerpo, "local_a")).await.unwrap();
    let encuentra = || async {
        mcp::buscar(&e, &busqueda("sesion cognito", "local_a")).await.unwrap().contains("[nota 1] decisión")
    };
    assert!(encuentra().await);

    // Como si las notas hubieran llegado de otra base: están, pero sin sus palabras.
    sqlx::query("delete from notas_texto").execute(&pool).await.unwrap();
    assert!(!encuentra().await);
    memoria::textos_al_dia(&pool).await.unwrap();
    assert!(encuentra().await, "al arrancar se arman las que faltan");

    // Si cambia la forma de sacar las raíces, lo guardado se arma de nuevo.
    sqlx::query("update notas_texto set titulo = 'otra', cuerpo = 'cosa'").execute(&pool).await.unwrap();
    memoria::textos_al_dia(&pool).await.unwrap();
    assert!(!encuentra().await, "con la misma forma no hay nada que rehacer");
    sqlx::query("update ajustes set valor = 'la-de-antes' where clave = 'raices'").execute(&pool).await.unwrap();
    memoria::textos_al_dia(&pool).await.unwrap();
    assert!(encuentra().await);
}

#[tokio::test]
async fn el_resumen_no_pasa_de_treinta_lineas() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_acme", "ACME", None).await;
    for i in 0..12 {
        for tipo in TipoNota::TODOS {
            mcp::guardar(&e, nota(tipo, &format!("{tipo} {i}"), &format!("Cuerpo {i}."), "local_a")).await.unwrap();
        }
    }
    mcp::guardar(&e, nota(TipoNota::Decision, "Regla de ACME", "Vale para toda ACME.", "local_acme")).await.unwrap();
    let r = empezar(&e, "local_a", "ACME", Some("Portal")).await;
    let lineas = r.lines().count();
    assert!(lineas <= 30, "{lineas} líneas:\n{r}");
    assert!(r.starts_with("Grupo: ACME › Portal · 72 notas vigentes"), "{r}");
    assert!(r.contains("Decisiones vigentes:") && r.contains("Pendientes:"), "{r}");
    assert!(r.contains("De ACME en general:\n- [nota 73] decisión"), "la empresa no queda afuera: {r}");
    assert!(r.ends_with("(Hay más: buscalas con memoria_buscar.)"), "{r}");
}
