//! El lector de conversaciones contra archivos de prueba con la misma forma
//! que los que guarda Claude Code.

mod comun;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use comun::{busqueda, estado, registro};
use memoria::estado::Estado;
use memoria::grupos::Registro;
use memoria::indice::Clase;
use memoria::lector::{self, REPOSO, Ubicaciones};
use memoria::mcp::{self, PedidoContexto};
use memoria::{grupos, huellas};
use serde_json::json;
use sqlx::SqlitePool;

const YA: Duration = Duration::ZERO;

/// Una carpeta como `~/.claude/projects`, vacía, para una prueba.
fn raiz(nombre: &str) -> PathBuf {
    let raiz = std::env::temp_dir().join(format!("memoria-lector-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&raiz);
    std::fs::create_dir_all(raiz.join("-Users-ana-GITHUB-acme-portal")).unwrap();
    raiz
}

fn archivo(raiz: &Path, transcripcion: &str) -> PathBuf {
    raiz.join("-Users-ana-GITHUB-acme-portal").join(format!("{transcripcion}.jsonl"))
}

fn agregar(archivo: &Path, texto: &str) {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(archivo).unwrap();
    f.write_all(texto.as_bytes()).unwrap();
}

fn del_usuario(texto: &str) -> String {
    let renglon = json!({
        "parentUuid": null, "isSidechain": false, "type": "user", "origin": { "kind": "human" },
        "timestamp": "2026-11-12T18:27:01.745Z", "sessionId": "abc", "cwd": "/Users/ana/GITHUB/acme/portal",
        "message": { "role": "user", "content": texto },
    });
    format!("{renglon}\n")
}

fn de_claude(texto: &str) -> String {
    let renglon = json!({
        "isSidechain": false, "type": "assistant", "timestamp": "2026-11-12T18:27:09.000Z",
        "message": { "model": "claude-opus-5-5", "role": "assistant", "content": [
            { "type": "thinking", "thinking": "a ver" },
            { "type": "text", "text": texto },
            { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "ls" } },
        ]},
    });
    format!("{renglon}\n")
}

/// Renglones que también hay en los archivos y que no son de la charla.
fn ruido() -> String {
    let renglones = [
        json!({ "type": "user", "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t1", "content": "RESULTADO-DE-HERRAMIENTA" }
        ]}}),
        json!({ "type": "user", "isMeta": true, "message": { "content": "AVISO-DE-LA-APP" } }),
        json!({ "type": "user", "origin": { "kind": "task-notification" }, "message": {
            "content": "<task-notification><status>TAREA-TERMINADA</status></task-notification>"
        }}),
        json!({ "type": "queue-operation", "operation": "enqueue", "content": "COLA" }),
        json!({ "type": "attachment", "attachment": { "type": "hook", "content": "ADJUNTO" } }),
        json!({ "type": "assistant", "isSidechain": true, "message": { "content": [{ "type": "text", "text": "SUBAGENTE" }] } }),
    ];
    renglones.iter().fold(String::new(), |mut junto, r| {
        let _ = writeln!(junto, "{r}");
        junto
    })
}

async fn registrar(e: &Estado, sesion: &str, proyecto: &str, transcripcion: &str) {
    let grupo_app = format!("[ACME] - {}", proyecto.to_uppercase());
    let datos = Registro { transcripcion: Some(transcripcion.into()), ..registro("ACME", Some(proyecto), &grupo_app) };
    mcp::contexto(e, &PedidoContexto { sesion: sesion.into(), grupo: None, registro: Some(datos) }).await.unwrap();
}

async fn contenidos(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar("select contenido from fragmentos order by id").fetch_all(pool).await.unwrap()
}

async fn leida_hasta(pool: &SqlitePool, sesion: &str) -> i64 {
    sqlx::query_scalar("select leida_hasta from conversaciones where id = ?1")
        .bind(sesion)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn lee_lo_conversado_y_la_busqueda_lo_encuentra() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let raiz = raiz("busqueda");
    let ruta = archivo(&raiz, "abc-123");
    registrar(&e, "local_a", "Portal", "abc-123").await;
    agregar(
        &ruta,
        &del_usuario("<system-reminder>RECORDATORIO</system-reminder>\n¿Con qué mandamos los mails del Inbox?"),
    );
    agregar(&ruta, &ruido());
    agregar(&ruta, &de_claude("Con SendGrid: producción comparte la cuenta de Tienda."));
    let mut conocidos = Ubicaciones::default();

    // Recién escrita, la conversación todavía está en uso: espera.
    assert_eq!(lector::recorrer(&e, &raiz, &mut conocidos, REPOSO).await.unwrap(), 0);
    assert_eq!(lector::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 1);
    assert_eq!(
        contenidos(&pool).await,
        [
            "Usuario: ¿Con qué mandamos los mails del Inbox?\n\nClaude: Con SendGrid: producción comparte la cuenta de Tienda."
        ]
    );
    let tamano = i64::try_from(std::fs::metadata(&ruta).unwrap().len()).unwrap();
    assert_eq!(leida_hasta(&pool, "local_a").await, tamano);
    let (grupo, fecha): (String, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("select g.proyecto, f.fecha from fragmentos f join grupos g on g.id = f.grupo_id")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(grupo, "Portal");
    assert_eq!(fecha.to_rfc3339(), "2026-11-12T18:27:01.745+00:00");

    // Por palabras se encuentra enseguida; por significado, cuando tiene huella.
    let r = mcp::buscar(&e, &busqueda("sendgrid tienda", "local_a")).await.unwrap();
    assert!(r.contains("[conversación local_a]") && r.contains("«Conversación de prueba»"), "{r}");
    assert_eq!(huellas::completar(&e).await.unwrap(), Some(1));
    let r = mcp::buscar(&e, &busqueda("correo", "local_a")).await.unwrap();
    assert!(r.contains("[conversación local_a]"), "«correo» y «mails» dicen lo mismo: {r}");
    // Compartir una palabra suelta no alcanza para mostrar un pedazo.
    let r = mcp::buscar(&e, &busqueda("tienda kubernetes", "local_a")).await.unwrap();
    assert!(r.starts_with("Nada para"), "{r}");
    // Si solo se piden notas, los pedazos no aparecen.
    let solo_notas = memoria::mcp::PedidoBuscar { conversaciones: false, ..busqueda("sendgrid", "local_a") };
    assert!(mcp::buscar(&e, &solo_notas).await.unwrap().starts_with("Nada para"));

    // Sin nada nuevo no hace nada. Lo que se agrega después se lee desde ahí.
    assert_eq!(lector::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 0);
    agregar(&ruta, &del_usuario("Anotá que los reportes salen los lunes a la mañana."));
    let a_medias = de_claude("Anotado: los reportes salen los lunes.");
    let (primera_mitad, segunda_mitad) = a_medias.split_at(a_medias.len() / 2);
    agregar(&ruta, primera_mitad);
    assert_eq!(lector::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 1);
    let hasta_el_renglon_entero = leida_hasta(&pool, "local_a").await;
    assert!(hasta_el_renglon_entero > tamano);
    // El renglón que estaba a medio escribir se lee cuando termina.
    agregar(&ruta, segunda_mitad);
    assert_eq!(lector::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 1);
    let todos = contenidos(&pool).await;
    assert_eq!(todos.len(), 3);
    assert_eq!(todos[1], "Usuario: Anotá que los reportes salen los lunes a la mañana.");
    assert_eq!(todos[2], "Claude: Anotado: los reportes salen los lunes.");
    let junto = todos.join("\n");
    for ajeno in [
        "RECORDATORIO",
        "RESULTADO-DE-HERRAMIENTA",
        "AVISO-DE-LA-APP",
        "TAREA-TERMINADA",
        "COLA",
        "ADJUNTO",
        "SUBAGENTE",
        "a ver",
    ] {
        assert!(!junto.contains(ajeno), "se coló «{ajeno}»");
    }
    let errores: i64 =
        sqlx::query_scalar("select count(*) from senales where tipo = 'error'").fetch_one(&pool).await.unwrap();
    assert_eq!(errores, 0, "el lector entendió todo");
    std::fs::remove_dir_all(&raiz).unwrap();
}

#[tokio::test]
async fn tapa_los_secretos_de_lo_conversado() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let raiz = raiz("secretos");
    let ruta = archivo(&raiz, "abc-456");
    registrar(&e, "local_a", "Portal", "abc-456").await;
    agregar(&ruta, &del_usuario("Probá el deploy con el token ghp_abcdefghijklmnopqrstuvwxyz0123456789 y contame."));
    assert_eq!(lector::procesar(&e, "local_a", &ruta).await.unwrap(), 1);
    assert_eq!(contenidos(&pool).await, ["Usuario: Probá el deploy con el token [secreto quitado] y contame."]);
    std::fs::remove_dir_all(&raiz).unwrap();
}

#[tokio::test]
async fn los_pedazos_siguen_a_la_conversacion() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let raiz = raiz("grupo");
    let ruta = archivo(&raiz, "abc-789");
    registrar(&e, "local_a", "Portal", "abc-789").await;
    agregar(&ruta, &del_usuario("El pipeline de ALFA ya está en producción desde el jueves."));
    assert_eq!(lector::procesar(&e, "local_a", &ruta).await.unwrap(), 1);
    let proyecto = || async {
        sqlx::query_scalar::<_, String>("select g.proyecto from fragmentos f join grupos g on g.id = f.grupo_id")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert_eq!(proyecto().await, "Portal");
    assert_eq!(huellas::completar(&e).await.unwrap(), Some(1));
    registrar(&e, "local_i", "Portal", "otra-portal").await;
    let por_significado = |sesion: &'static str| {
        let e = e.clone();
        async move { mcp::buscar(&e, &busqueda("pipeline produccion jueves alfa", sesion)).await.unwrap() }
    };
    let r = por_significado("local_i").await;
    assert!(r.contains("[conversación local_a]") && !r.contains("· de ACME"), "es del mismo grupo: {r}");

    // El usuario la arrastra a otro grupo del costado: el mod la registra de nuevo.
    registrar(&e, "local_a", "Tienda", "abc-789").await;
    assert_eq!(proyecto().await, "Tienda");
    // Las huellas en memoria también cambian de grupo: desde Portal ya no es
    // de la casa (hay que ampliar a la empresa para verla).
    let r = por_significado("local_i").await;
    assert!(r.contains("[conversación local_a]") && r.contains("· de ACME › Tienda"), "{r}");
    assert!(leida_hasta(&pool, "local_a").await > 0, "mismo archivo: no se relee");

    // Después de un /clear la conversación sigue en otro archivo, desde cero.
    registrar(&e, "local_a", "Tienda", "nuevo-000").await;
    assert_eq!(leida_hasta(&pool, "local_a").await, 0);
    let otro = archivo(&raiz, "nuevo-000");
    agregar(&otro, &del_usuario("Otra cosa: el Inbox de Tienda usa la cola de SQS para los recordatorios."));
    agregar(&otro, &de_claude("Anotado. La cola de SQS manda veinte recordatorios por segundo como mucho."));
    assert_eq!(lector::recorrer(&e, &raiz, &mut Ubicaciones::default(), YA).await.unwrap(), 1);
    assert_eq!(contenidos(&pool).await.len(), 2, "lo del archivo anterior se conserva");

    // Si el archivo se achica (es otro), se lee de nuevo entero sin duplicar.
    std::fs::write(&otro, del_usuario("El archivo se escribió de nuevo, con otra charla.")).unwrap();
    assert_eq!(lector::recorrer(&e, &raiz, &mut Ubicaciones::default(), YA).await.unwrap(), 1);
    assert_eq!(contenidos(&pool).await, ["Usuario: El archivo se escribió de nuevo, con otra charla."]);
    // Lo borrado tampoco queda en las palabras ni en las huellas en memoria.
    let con_palabras: i64 =
        sqlx::query_scalar("select count(*) from fragmentos_texto_docsize").fetch_one(&pool).await.unwrap();
    assert_eq!(con_palabras, 1);
    assert_eq!(e.indice.cuantas(Clase::Fragmentos), 0);
    assert_eq!(huellas::completar(&e).await.unwrap(), Some(1));
    assert_eq!(e.indice.cuantas(Clase::Fragmentos), 1);
    let r = mcp::buscar(&e, &busqueda("pipeline produccion jueves alfa", "local_a")).await.unwrap();
    assert!(r.starts_with("Nada para"), "lo borrado no aparece más: {r}");
    std::fs::remove_dir_all(&raiz).unwrap();
}

#[tokio::test]
async fn una_pregunta_larga_encuentra_el_pedazo_por_palabras() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    // Sin huellas: lo que aparece, aparece por palabras.
    let e = comun::estado_sin_huellas(pool.clone()).await;
    let raiz = raiz("larga");
    registrar(&e, "local_a", "Portal", "abc-321").await;
    agregar(&archivo(&raiz, "abc-321"), &del_usuario("¿Con qué mandamos los mails del Inbox?"));
    agregar(&archivo(&raiz, "abc-321"), &de_claude("Con SendGrid: producción comparte la cuenta de Tienda."));
    registrar(&e, "local_b", "Portal", "abc-654").await;
    let otra = "El Inbox de soporte quedó vacío y la cuenta de producción se revisa el lunes.";
    agregar(&archivo(&raiz, "abc-654"), &del_usuario(otra));
    assert_eq!(lector::recorrer(&e, &raiz, &mut Ubicaciones::default(), YA).await.unwrap(), 2);

    // Cinco palabras con peso (servicio, mandamos, mails, inbox, producción):
    // alcanza con cuatro. El otro pedazo tiene dos y no aparece.
    let larga = "con qué servicio mandamos los mails del inbox en producción";
    let r = mcp::buscar(&e, &busqueda(larga, "local_a")).await.unwrap();
    assert!(r.starts_with("1 resultado"), "{r}");
    assert!(r.contains("[conversación local_a]") && r.contains("SendGrid"), "{r}");
    // Seis palabras, están cinco: también.
    let otra_larga = "qué cuenta de sendgrid comparte tienda en producción con portal";
    let r = mcp::buscar(&e, &busqueda(otra_larga, "local_a")).await.unwrap();
    assert!(r.starts_with("1 resultado") && r.contains("[conversación local_a]"), "{r}");
    // Seis palabras y solo están dos: no alcanza.
    let lejana = "servicio de kubernetes para mandar reportes semanales del inbox";
    let r = mcp::buscar(&e, &busqueda(lejana, "local_a")).await.unwrap();
    assert!(r.starts_with("Nada para"), "{r}");
    // Con hasta tres palabras tienen que estar todas.
    let r = mcp::buscar(&e, &busqueda("inbox producción", "local_a")).await.unwrap();
    assert!(r.starts_with("2 resultados"), "{r}");
    let r = mcp::buscar(&e, &busqueda("inbox producción sendgrid", "local_a")).await.unwrap();
    assert!(r.starts_with("1 resultado"), "{r}");
    std::fs::remove_dir_all(&raiz).unwrap();
}

#[tokio::test]
async fn la_copia_de_una_conversacion_no_repite_los_pedazos() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let raiz = raiz("copia");
    let charla = [
        del_usuario("¿Qué día se le entrega al cliente el informe de turnos?"),
        de_claude("Los viernes antes del mediodía, según lo acordado con el cliente."),
    ]
    .concat();
    registrar(&e, "local_a", "Portal", "original-1").await;
    agregar(&archivo(&raiz, "original-1"), &charla);
    // La copia arranca con todo lo de la original y después sigue sola.
    registrar(&e, "local_b", "Portal", "copia-2").await;
    agregar(&archivo(&raiz, "copia-2"), &charla);
    let mut conocidos = Ubicaciones::default();
    assert_eq!(lector::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 1);
    assert_eq!(contenidos(&pool).await.len(), 1, "lo repetido se guarda una sola vez");
    agregar(&archivo(&raiz, "copia-2"), &del_usuario("En la copia: sumemos también el informe de cancelaciones."));
    assert_eq!(lector::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 1);
    assert_eq!(contenidos(&pool).await.len(), 2);
    // El mismo texto en otro grupo sí se guarda: allá también tiene que encontrarse.
    registrar(&e, "local_f", "Tienda", "otra-3").await;
    agregar(&archivo(&raiz, "otra-3"), &charla);
    assert_eq!(lector::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 1);
    std::fs::remove_dir_all(&raiz).unwrap();
}

#[tokio::test]
async fn lo_que_no_entiende_deja_una_senal() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let raiz = raiz("senal");
    let ruta = archivo(&raiz, "abc-999");
    registrar(&e, "local_a", "Portal", "abc-999").await;
    agregar(&ruta, "{\"type\":\"user\",\"message\":{\"content\":42}}\n");
    agregar(&ruta, "{\"type\":\"user\",\"message\":{\"content\": esto no es json\n");
    agregar(&ruta, &del_usuario("Esto sí se entiende: la reunión con el cliente pasa al martes."));
    assert_eq!(lector::procesar(&e, "local_a", &ruta).await.unwrap(), 1, "lo que se entiende se guarda igual");
    let (tipo, conversacion, origen, motivo): (String, Option<String>, String, String) =
        sqlx::query_as("select tipo, conversacion, detalle->>'origen', detalle->>'motivo' from senales")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((tipo.as_str(), conversacion.as_deref(), origen.as_str()), ("error", Some("local_a"), "lector"));
    assert!(motivo.starts_with("2 renglones"), "{motivo}");
    std::fs::remove_dir_all(&raiz).unwrap();
}

#[tokio::test]
async fn solo_lee_archivos_de_la_carpeta_de_conversaciones() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let e = estado(pool.clone()).await;
    let raiz = raiz("afuera");
    // Un archivo fuera de la carpeta, al que un id mal armado querría llegar.
    let afuera = raiz.parent().unwrap().join(format!("memoria-afuera-{}.jsonl", std::process::id()));
    std::fs::write(&afuera, del_usuario("Esto está fuera de la carpeta de conversaciones y no se tiene que leer."))
        .unwrap();
    let id_malo = format!("../../{}", afuera.file_stem().unwrap().to_string_lossy());
    registrar(&e, "local_a", "Portal", &id_malo).await;
    // Un subagente: está más adentro que las conversaciones.
    let adentro = raiz.join("-Users-ana-GITHUB-acme-portal").join("abc-111").join("subagents");
    std::fs::create_dir_all(&adentro).unwrap();
    std::fs::write(
        adentro.join("agent-1.jsonl"),
        del_usuario("Esto es de un subagente y tampoco se tiene que leer nunca."),
    )
    .unwrap();
    registrar(&e, "local_b", "Portal", "agent-1").await;
    // Y una conversación sin archivo todavía.
    registrar(&e, "local_c", "Portal", "sin-archivo").await;
    assert_eq!(lector::recorrer(&e, &raiz, &mut Ubicaciones::default(), YA).await.unwrap(), 0);
    assert_eq!(contenidos(&pool).await, Vec::<String>::new());
    assert!(grupos::de_conversacion(&pool, "local_a").await.unwrap().is_some());
    std::fs::remove_file(&afuera).unwrap();
    std::fs::remove_dir_all(&raiz).unwrap();
}
