//! Etapa 2: búsqueda por significado, unión de repetidas y reemplazos. Usan
//! unas huellas de mentira (por palabras); la última prueba corre contra
//! Ollama con el modelo de verdad, si está prendido.

mod comun;

use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::post;
use axum::{Json, Router};
use comun::{busqueda, empezar, estado, estado_sin_huellas, nota};
use memoria::error::Error;
use memoria::estado::Estado;
use memoria::huellas::{self, DIMENSIONES, Falla, Huellas, Ollama, Uso};
use memoria::indice::Clase;
use memoria::interno::Interno;
use memoria::mcp::{self, PedidoReemplazar};
use memoria::modelo::TipoNota;
use serde_json::{Value, json};
use sqlx::SqlitePool;

const ERROR_SESION: (&str, &str) = (
    "Error de sesión en Cognito",
    "Error: a la hora de entrar, la app volvía a pedir el usuario. Causa: el refresh token duraba 60 minutos. Solución: subir la validez a 30 días en el user pool.",
);

async fn cuantas_notas(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("select count(*) from notas").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn encuentra_lo_que_se_guardo_con_otras_palabras() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    mcp::guardar(&e, nota(TipoNota::Error, ERROR_SESION.0, ERROR_SESION.1, "local_a")).await.unwrap();
    mcp::guardar(
        &e,
        nota(TipoNota::Acuerdo, "Entregas los viernes", "Acordado con el cliente: se entrega cada viernes.", "local_a"),
    )
    .await
    .unwrap();

    // Ni «problema» ni «login» están en la nota: la encuentra por significado.
    let r = mcp::buscar(&e, &busqueda("el problema del login", "local_a")).await.unwrap();
    assert!(r.starts_with("1 resultado para «el problema del login» en ACME › Portal"), "{r}");
    assert!(r.contains("[nota 1] error · vigente") && r.contains("Error de sesión en Cognito"), "{r}");
    assert!(!r.contains("Entregas"), "lo que no tiene que ver no aparece: {r}");

    // Algo que no se parece a nada sigue dando vacío (y deja su señal).
    let r = mcp::buscar(&e, &busqueda("receta de tarta", "local_a")).await.unwrap();
    assert!(r.starts_with("Nada para «receta de tarta»"), "{r}");
    let por_significado: bool =
        sqlx::query_scalar("select detalle ->> 'por_significado' from senales").fetch_one(&pool).await.unwrap();
    assert!(por_significado);

    // Las huellas están guardadas en la base: después de reiniciar el
    // servicio se cargan de nuevo y la búsqueda por significado sigue andando.
    let reiniciado = estado(pool.clone()).await;
    assert_eq!(reiniciado.indice.cuantas(Clase::Notas), 2);
    let r = mcp::buscar(&reiniciado, &busqueda("el problema del login", "local_a")).await.unwrap();
    assert!(r.contains("[nota 1] error · vigente"), "{r}");
}

#[tokio::test]
async fn lo_que_coincide_por_palabras_y_por_significado_va_primero() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    // Las dos hablan de la sesión; solo la segunda, además, dice «vencida».
    mcp::guardar(&e, nota(TipoNota::Dato, "Proveedor de login", "El login lo da Cognito.", "local_a")).await.unwrap();
    mcp::guardar(
        &e,
        nota(TipoNota::Error, "Sesión vencida al volver", "La sesión vencida pedía entrar otra vez.", "local_a"),
    )
    .await
    .unwrap();
    let r = mcp::buscar(&e, &busqueda("sesión vencida", "local_a")).await.unwrap();
    let segunda = r.find("[nota 2]").expect(&r);
    let primera = r.find("[nota 1]").expect(&r);
    assert!(segunda < primera, "{r}");
}

#[tokio::test]
async fn una_palabra_suelta_no_alcanza() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_tcp", "ACME", None).await;
    // En el grupo hay tres notas que con la consulta solo comparten «exportar».
    for cosa in ["turnos", "reportes", "usuarios"] {
        let titulo = format!("Exportar {cosa} a Excel");
        let cuerpo = format!("Los {cosa} se exportan a planilla.");
        mcp::guardar(&e, nota(TipoNota::Dato, &titulo, &cuerpo, "local_a")).await.unwrap();
    }
    // La que responde de verdad está guardada para toda la empresa.
    mcp::guardar(
        &e,
        nota(TipoNota::Decision, "Formato de las facturas", "Las facturas se exportan en PDF firmado.", "local_tcp"),
    )
    .await
    .unwrap();

    // Las del grupo no frenan la búsqueda: sigue hasta la empresa y pone
    // primero la que tiene todas las palabras. Las otras solo completan.
    let r = mcp::buscar(&e, &busqueda("exportar facturas firmadas", "local_a")).await.unwrap();
    assert!(
        r.starts_with(
            "3 resultados para «exportar facturas firmadas» en ACME › Portal (había pocos: amplié a ACME y a todo)"
        ),
        "{r}"
    );
    let primera = r.lines().nth(1).unwrap();
    assert!(primera.starts_with("1. [nota 4] decisión · vigente") && primera.contains("· de ACME"), "{r}");
    assert!(!primera.contains("coincide solo en parte"), "{r}");
    assert_eq!(r.matches("coincide solo en parte").count(), 2, "{r}");
}

#[tokio::test]
async fn guardar_lo_mismo_con_otras_palabras_deja_una_sola_nota() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_b", "ACME", Some("Portal")).await;
    mcp::guardar(
        &e,
        nota(TipoNota::Acuerdo, "Entregas los viernes", "Acordado con el cliente: se entrega cada viernes.", "local_a"),
    )
    .await
    .unwrap();
    let mut otra = nota(
        TipoNota::Acuerdo,
        "Entregas cada viernes",
        "Se entrega cada viernes: acordado con el cliente.",
        "local_b",
    );
    otra.etiquetas = vec!["Cliente".into()];
    let r = mcp::guardar(&e, otra).await.unwrap();
    assert!(
        r.starts_with("Ya había una nota que decía lo mismo (nota 1 en ACME › Portal, «Entregas los viernes»)"),
        "{r}"
    );

    assert_eq!(cuantas_notas(&pool).await, 1);
    let (titulo, origenes, etiquetas): (String, String, String) =
        sqlx::query_as("select titulo, origenes, etiquetas from notas").fetch_one(&pool).await.unwrap();
    assert_eq!(titulo, "Entregas cada viernes", "queda el texto nuevo");
    let (mut origenes, mut etiquetas) = (memoria::base::lista(&origenes), memoria::base::lista(&etiquetas));
    origenes.sort();
    etiquetas.sort();
    assert_eq!(origenes, vec!["local_a", "local_b"]);
    assert_eq!(etiquetas, vec!["auth", "cliente"]);
    // La huella es la del texto nuevo: se sigue encontrando.
    let r = mcp::buscar(&e, &busqueda("cuándo se entrega", "local_a")).await.unwrap();
    assert!(r.contains("[nota 1] acuerdo · vigente"), "{r}");
}

#[tokio::test]
async fn si_se_parece_la_guarda_y_avisa_a_cual() {
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
    let r = mcp::guardar(
        &e,
        nota(
            TipoNota::Acuerdo,
            "Los viernes hay entregas",
            "Con el cliente quedó acordado: cada viernes se entrega.",
            "local_a",
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        r,
        "Guardada como nota 2 en ACME › Portal (acuerdo). Se parece a la nota 1 («Entregas los viernes»): si esta la corrige o la reemplaza, llamá a memoria_reemplazar con vieja 1 y nueva 2."
    );
    // Algo de otro tema se guarda sin aviso.
    let r = mcp::guardar(&e, nota(TipoNota::Dato, "Región de los deploys", "Los deploys van a us-east-1.", "local_a"))
        .await
        .unwrap();
    assert_eq!(r, "Guardada como nota 3 en ACME › Portal (dato).");
    // Y lo mismo en otro grupo es otra nota: las repetidas se miran dentro del grupo.
    empezar(&e, "local_f", "ACME", Some("Tienda")).await;
    let r = mcp::guardar(
        &e,
        nota(
            TipoNota::Acuerdo,
            "Entregas cada viernes",
            "Se entrega cada viernes: acordado con el cliente.",
            "local_f",
        ),
    )
    .await
    .unwrap();
    assert_eq!(r, "Guardada como nota 4 en ACME › Tienda (acuerdo).");
}

const TAREAS: &str = "por el trabajo de infraestructura, monitoreo, despliegues, bases, redes, reportes, reuniones, \
documentación, guardias, soporte, migraciones, alarmas, tableros, permisos, respaldos, pruebas y revisiones";

#[tokio::test]
async fn casi_iguales_pero_con_otro_dato_son_dos_notas() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "Globex", None).await;
    let octubre = format!("En octubre se facturaron 62 horas {TAREAS}.");
    let noviembre = format!("En noviembre se facturaron 48 horas {TAREAS}.");
    mcp::guardar(&e, nota(TipoNota::Dato, "Facturación del mes", &octubre, "local_a")).await.unwrap();
    let r = mcp::guardar(&e, nota(TipoNota::Dato, "Facturación del mes", &noviembre, "local_a")).await.unwrap();
    assert!(r.starts_with("Guardada como nota 2 en Globex (dato). Se parece a la nota 1"), "{r}");
    assert_eq!(cuantas_notas(&pool).await, 2, "cambian el mes y las horas: son dos hechos");

    // Otro tipo de nota tampoco se pisa.
    let r = mcp::guardar(&e, nota(TipoNota::Pendiente, "Facturación del mes", &octubre, "local_a")).await.unwrap();
    assert!(r.starts_with("Guardada como nota 3 en Globex (pendiente). Se parece a la nota 1"), "{r}");

    // La misma, con una frase de más y los mismos datos, sí se une.
    let ampliada = format!("{octubre} Se envió el detalle.");
    let r = mcp::guardar(&e, nota(TipoNota::Dato, "Facturación del mes", &ampliada, "local_a")).await.unwrap();
    assert!(r.starts_with("Ya había una nota que decía lo mismo (nota 1 en Globex"), "{r}");
    assert_eq!(cuantas_notas(&pool).await, 3);
}

#[tokio::test]
async fn reemplazar_deja_la_vieja_como_historia() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    mcp::guardar(&e, nota(TipoNota::Decision, "Mails con Mailgun", "Qué: los mails salen por Mailgun.", "local_a"))
        .await
        .unwrap();
    mcp::guardar(
        &e,
        nota(TipoNota::Decision, "Correo por SES", "Qué: el correo pasa a SES. Por qué: costo.", "local_a"),
    )
    .await
    .unwrap();
    let r = mcp::reemplazar(&e, &PedidoReemplazar { vieja: 1, nueva: 2 }).await.unwrap();
    assert_eq!(
        r,
        "La nota 1 («Mails con Mailgun») quedó como reemplazada por la nota 2 («Correo por SES»), que es la que vale ahora."
    );
    let (estado_vieja, por): (String, Option<i64>) =
        sqlx::query_as("select estado, reemplazada_por from notas where id = 1").fetch_one(&pool).await.unwrap();
    assert_eq!((estado_vieja.as_str(), por), ("reemplazada", Some(2)));

    // Buscar lo viejo lleva a lo que vale hoy.
    let r = mcp::buscar(&e, &busqueda("mailgun", "local_a")).await.unwrap();
    assert!(r.contains("[nota 2] decisión · vigente"), "{r}");
    assert!(r.contains("Antes se usaba: [nota 1] Mails con Mailgun (reemplazada)"), "{r}");
    // El resumen del grupo solo trae la vigente.
    let resumen = empezar(&e, "local_a", "ACME", Some("Portal")).await;
    assert!(resumen.contains("Correo por SES") && !resumen.contains("Mailgun"), "{resumen}");

    let errores = [
        (PedidoReemplazar { vieja: 1, nueva: 2 }, "La nota 1 ya está reemplazada"),
        (PedidoReemplazar { vieja: 2, nueva: 1 }, "La nota 1 está reemplazada: no puede reemplazar a otra"),
        (PedidoReemplazar { vieja: 2, nueva: 2 }, "no puede reemplazarse a sí misma"),
        (PedidoReemplazar { vieja: 2, nueva: 99 }, "No existe la nota 99"),
        (PedidoReemplazar { vieja: 98, nueva: 2 }, "No existe la nota 98"),
    ];
    for (pedido, esperado) in errores {
        let Err(Error::Uso(m)) = mcp::reemplazar(&e, &pedido).await else { panic!("debía rechazarlo: {esperado}") };
        assert!(m.contains(esperado), "{m}");
    }
}

#[tokio::test]
async fn sigue_la_cadena_de_reemplazos_hasta_la_vigente() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    let pasos =
        [("Deploy con Jenkins", "Jenkins"), ("Deploy con CircleCI", "CircleCI"), ("Deploy con Actions", "Actions")];
    for (i, (titulo, con)) in pasos.iter().enumerate() {
        let mut n = nota(TipoNota::Decision, titulo, &format!("Qué: se despliega con {con}."), "local_a");
        n.reemplaza = i64::try_from(i).ok().filter(|i| *i > 0);
        mcp::guardar(&e, n).await.unwrap();
    }
    let r = mcp::buscar(&e, &busqueda("jenkins", "local_a")).await.unwrap();
    assert!(r.contains("[nota 3] decisión · vigente") && r.contains("— Deploy con Actions"), "{r}");
    assert!(r.contains("Antes se usaba: [nota 1] Deploy con Jenkins (reemplazada)"), "{r}");
    assert!(!r.contains("[nota 2]"), "{r}");
}

#[tokio::test]
async fn sin_huellas_anda_por_palabras_y_despues_se_completan() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let sin = estado_sin_huellas(pool.clone()).await;
    assert_eq!(sin.huellas_andan(), None);
    empezar(&sin, "local_a", "ACME", Some("Portal")).await;
    let r = mcp::guardar(&sin, nota(TipoNota::Error, ERROR_SESION.0, ERROR_SESION.1, "local_a")).await.unwrap();
    assert_eq!(r, "Guardada como nota 1 en ACME › Portal (error).");
    assert_eq!(sin.huellas_andan(), Some(false));
    let r = mcp::buscar(&sin, &busqueda("sesion cognito", "local_a")).await.unwrap();
    assert!(r.contains("[nota 1] error · vigente"), "por palabras sigue andando: {r}");
    let r = mcp::buscar(&sin, &busqueda("el problema del login", "local_a")).await.unwrap();
    assert!(r.starts_with("Nada para"), "{r}");
    assert_eq!(huellas::completar(&sin).await.unwrap(), None, "sin Ollama no se completa nada");

    // Cuando las huellas vuelven, la tarea de fondo completa lo que faltaba.
    let con = estado(pool.clone()).await;
    assert_eq!(huellas::completar(&con).await.unwrap(), Some(1));
    assert_eq!(huellas::completar(&con).await.unwrap(), Some(0));
    assert_eq!(con.huellas_andan(), Some(true));
    let r = mcp::buscar(&con, &busqueda("el problema del login", "local_a")).await.unwrap();
    assert!(r.contains("[nota 1] error · vigente"), "{r}");
}

#[tokio::test]
async fn si_cambia_el_modelo_las_huellas_se_calculan_de_nuevo() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let con_huella = || async {
        sqlx::query_scalar::<_, i64>("select count(*) from notas where huella is not null")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    huellas::al_arrancar(&pool, e.huellas.modelo()).await.unwrap();
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    mcp::guardar(&e, nota(TipoNota::Error, ERROR_SESION.0, ERROR_SESION.1, "local_a")).await.unwrap();
    assert_eq!(con_huella().await, 1);
    huellas::al_arrancar(&pool, e.huellas.modelo()).await.unwrap();
    assert_eq!(con_huella().await, 1, "mismo modelo: no se toca nada");
    huellas::al_arrancar(&pool, "otro-modelo").await.unwrap();
    assert_eq!(con_huella().await, 0, "las de otro modelo no se pueden comparar");
    assert_eq!(huellas::completar(&e).await.unwrap(), Some(1));
    assert_eq!(con_huella().await, 1);
}

/// Un Ollama de mentira en un puerto libre, que contesta lo que diga `responder`.
async fn ollama_de_mentira(responder: fn(&Value) -> (axum::http::StatusCode, Value)) -> String {
    let rutas = Router::new().route(
        "/api/embed",
        post(move |Json(pedido): Json<Value>| async move {
            let (estado, cuerpo) = responder(&pedido);
            (estado, Json(cuerpo))
        }),
    );
    let escucha = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direccion = format!("http://{}", escucha.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(escucha, rutas).await.unwrap() });
    direccion
}

#[tokio::test]
async fn habla_con_ollama_y_revisa_lo_que_vuelve() {
    let textos = vec!["uno".to_owned(), "dos".to_owned()];

    let bien = ollama_de_mentira(|pedido| {
        assert_eq!(pedido["model"], "bge-m3");
        assert_eq!(pedido["truncate"], true);
        let cuantos = pedido["input"].as_array().unwrap().len();
        (axum::http::StatusCode::OK, json!({ "embeddings": vec![vec![0.5_f32; DIMENSIONES]; cuantos] }))
    })
    .await;
    let ollama = Ollama::nuevo(&format!("{bien}/"), "bge-m3").unwrap();
    assert_eq!(ollama.modelo(), "bge-m3");
    let huellas = ollama.calcular(&textos, Uso::Texto).await.unwrap();
    assert_eq!((huellas.len(), huellas[0].len()), (2, DIMENSIONES));
    assert_eq!(ollama.calcular(&[], Uso::Texto).await.unwrap(), Vec::<Vec<f32>>::new());

    let sin_modelo =
        ollama_de_mentira(|_| (axum::http::StatusCode::NOT_FOUND, json!({ "error": "model \"bge-m3\" not found" })))
            .await;
    let falla = Ollama::nuevo(&sin_modelo, "bge-m3").unwrap().calcular(&textos, Uso::Texto).await.unwrap_err();
    assert!(matches!(&falla, Falla::Respuesta(m) if m.contains("404") && m.contains("not found")), "{falla}");

    let otro_largo =
        ollama_de_mentira(|_| (axum::http::StatusCode::OK, json!({ "embeddings": [[0.1, 0.2], [0.3, 0.4]] }))).await;
    let falla = Ollama::nuevo(&otro_largo, "chico").unwrap().calcular(&textos, Uso::Texto).await.unwrap_err();
    assert!(matches!(&falla, Falla::Respuesta(m) if m.contains("huellas de 2 números")), "{falla}");

    let de_menos = ollama_de_mentira(|_| (axum::http::StatusCode::OK, json!({ "embeddings": [] }))).await;
    let falla = Ollama::nuevo(&de_menos, "bge-m3").unwrap().calcular(&textos, Uso::Texto).await.unwrap_err();
    assert!(matches!(&falla, Falla::Respuesta(m) if m.contains("pedí 2 huellas y llegaron 0")), "{falla}");

    // Nadie escucha en ese puerto.
    let falla = Ollama::nuevo("http://127.0.0.1:9", "bge-m3").unwrap().calcular(&textos, Uso::Texto).await.unwrap_err();
    assert!(matches!(falla, Falla::Conexion(_)), "{falla}");
}

/// La condición de la etapa 2, con `bge-m3` de verdad en Ollama. Si Ollama
/// no está prendido en esta máquina (o no tiene el modelo), la prueba no
/// hace nada.
#[tokio::test]
async fn con_bge_m3_en_ollama_el_login_encuentra_la_sesion_de_cognito() {
    let base = comun::Base::nueva().await;
    let direccion = std::env::var("MEMORIA_OLLAMA").unwrap_or_else(|_| "http://127.0.0.1:11434".to_owned());
    let ollama = Ollama::nuevo(&direccion, "bge-m3").unwrap();
    if let Err(falla) = ollama.calcular(&["hola".to_owned()], Uso::Texto).await {
        eprintln!("sin Ollama con bge-m3 ({falla}): salteo la prueba con el modelo de verdad");
        return;
    }
    con_un_modelo_de_verdad(Estado::nuevo(base.pool.clone(), Arc::new(ollama))).await;
}

/// Lo mismo con el modelo que corre dentro del programa. Solo si ya está
/// bajado en esta máquina (`MEMORIA_MODELOS` o `~/.memoria/modelos`): las
/// pruebas nunca bajan nada.
#[tokio::test]
async fn con_el_modelo_interno_el_login_encuentra_la_sesion_de_cognito() {
    let carpeta = std::env::var_os("MEMORIA_MODELOS")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|casa| PathBuf::from(casa).join(".memoria/modelos")));
    let Some(carpeta) = carpeta else { return };
    let interno = Interno::nuevo("gemma", &carpeta, 4).unwrap();
    if !interno.bajado() {
        eprintln!("el modelo interno no está bajado en {}: salteo la prueba", carpeta.display());
        return;
    }
    let base = comun::Base::nueva().await;
    con_un_modelo_de_verdad(Estado::nuevo(base.pool.clone(), Arc::new(interno))).await;
}

/// Lo que tiene que pasar con cualquier modelo de huellas que se ofrezca:
/// encontrar lo guardado con otras palabras, no inventar resultados y
/// reconocer la misma nota escrita distinto.
async fn con_un_modelo_de_verdad(e: Estado) {
    let pool = e.pool.clone();
    // Primero se le da tiempo al modelo para cargarse: después, cada huella
    // tiene que llegar dentro de lo que espera una búsqueda de verdad. Sin
    // esto, en una máquina ocupada la primera búsqueda se queda sin huella y
    // la prueba falla por lenta, no por lo que mide.
    e.huella_de("para que el modelo ya esté cargado", Uso::Texto, std::time::Duration::from_mins(3))
        .await
        .expect("el modelo de huellas tiene que responder");
    empezar(&e, "local_a", "ACME", Some("Portal")).await;
    empezar(&e, "local_b", "ACME", Some("Portal")).await;
    let notas = [
        (TipoNota::Error, ERROR_SESION.0, ERROR_SESION.1),
        (
            TipoNota::Acuerdo,
            "Entregas los viernes",
            "Acordado con el cliente: se entrega cada viernes antes del mediodía.",
        ),
        (
            TipoNota::Error,
            "Deploy falla por la región",
            "Error: el deploy usa us-east-2. Causa: perfil viejo de AWS. Solución: fijar AWS_REGION=us-east-1 en el pipeline.",
        ),
        (
            TipoNota::Dato,
            "Facturación de octubre de Globex",
            "Se facturaron 62 horas a 45 USD. La factura se mandó el 3/11 por mail a finanzas.",
        ),
        (
            TipoNota::Decision,
            "Migración de Aurora a DSQL",
            "Qué: la base pasa de Aurora Serverless a DSQL. Por qué: costo en reposo. Cómo: drizzle-kit push.",
        ),
    ];
    for (i, (tipo, titulo, cuerpo)) in notas.iter().enumerate() {
        let r = mcp::guardar(&e, nota(*tipo, titulo, cuerpo, "local_a")).await.unwrap();
        assert!(r.starts_with(&format!("Guardada como nota {} en ACME › Portal", i + 1)), "{r}");
    }

    // «El problema del login» encuentra «error de sesión en Cognito», primero.
    let r = mcp::buscar(&e, &busqueda("el problema del login", "local_b")).await.unwrap();
    assert!(r.lines().nth(1).is_some_and(|l| l.starts_with("1. [nota 1] error · vigente")), "{r}");
    for (consulta, esperada) in [
        ("qué día le entregamos al cliente", "[nota 2]"),
        ("en qué región desplegamos", "[nota 3]"),
        ("cuántas horas se cobraron", "[nota 4]"),
        ("qué base de datos usamos", "[nota 5]"),
    ] {
        let r = mcp::buscar(&e, &busqueda(consulta, "local_b")).await.unwrap();
        assert!(r.lines().nth(1).is_some_and(|l| l.contains(esperada)), "«{consulta}»: {r}");
    }
    let r = mcp::buscar(&e, &busqueda("receta de tarta de manzana", "local_b")).await.unwrap();
    assert!(r.starts_with("Nada para"), "{r}");

    // Guardar dos veces lo mismo, con otras palabras, deja una sola nota.
    let r = mcp::guardar(
        &e,
        nota(
            TipoNota::Error,
            "Cognito cerraba la sesión cada hora",
            "Error: la app volvía a pedir el usuario a la hora. Causa: refresh token de 60 minutos. Solución: validez de 30 días en el user pool.",
            "local_b",
        ),
    )
    .await
    .unwrap();
    assert!(r.starts_with("Ya había una nota que decía lo mismo (nota 1 en ACME › Portal"), "{r}");
    // La de noviembre se parece a la de octubre, pero es otro hecho.
    let r = mcp::guardar(
        &e,
        nota(
            TipoNota::Dato,
            "Facturación de noviembre de Globex",
            "Se facturaron 48 horas a 45 USD. La factura se mandó el 2/12 por mail a finanzas.",
            "local_b",
        ),
    )
    .await
    .unwrap();
    assert!(r.starts_with("Guardada como nota 6 en ACME › Portal (dato). Se parece a la nota 4"), "{r}");
    assert_eq!(cuantas_notas(&pool).await, 6);
}
