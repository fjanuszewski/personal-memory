//! Estilo del usuario de punta a punta: el servicio junta lo que él escribió
//! en sus conversaciones con Claude y `memoria_estilo` cuenta cómo escribe.

mod comun;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use comun::{estado, registro};
use memoria::error::Error;
use memoria::estado::Estado;
use memoria::estilo;
use memoria::grupos::Registro;
use memoria::lector::{REPOSO, Ubicaciones};
use memoria::mcp::{self, PedidoContexto, PedidoEstilo};
use serde_json::{Value, json};
use sqlx::SqlitePool;

const YA: Duration = Duration::ZERO;

/// Una carpeta como `~/.claude/projects`, vacía, para una prueba.
fn raiz(nombre: &str) -> PathBuf {
    let raiz = std::env::temp_dir().join(format!("memoria-estilo-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&raiz);
    std::fs::create_dir_all(raiz.join("-Users-ana-GITHUB-acme-portal")).unwrap();
    raiz
}

fn archivo(raiz: &Path, transcripcion: &str) -> PathBuf {
    raiz.join("-Users-ana-GITHUB-acme-portal").join(format!("{transcripcion}.jsonl"))
}

fn escribir(archivo: &Path, renglones: &[Value]) {
    let mut f = std::fs::File::create(archivo).unwrap();
    for renglon in renglones {
        writeln!(f, "{renglon}").unwrap();
    }
}

fn del_usuario(texto: &str, minuto: u32) -> Value {
    json!({
        "isSidechain": false, "type": "user", "origin": { "kind": "human" },
        "timestamp": format!("2026-11-12T18:{minuto:02}:00.000Z"),
        "message": { "role": "user", "content": texto },
    })
}

fn de_claude(texto: &str, botones: &[&str]) -> Value {
    let mut bloques = vec![json!({ "type": "text", "text": texto })];
    if !botones.is_empty() {
        let acciones: Vec<Value> = botones.iter().map(|b| json!({ "label": b })).collect();
        bloques.push(json!({ "type": "tool_use", "name": "mcp__ccd_turn__wrap_up",
                             "input": { "recap": "Quedó listo para subir.", "actions": acciones } }));
    }
    json!({ "isSidechain": false, "type": "assistant", "message": { "model": "claude-opus-5-5", "content": bloques } })
}

/// Una conversación de trabajo: ocho mensajes que escribió el usuario, y en
/// el medio lo que no escribió él.
fn conversacion() -> Vec<Value> {
    vec![
        // El encargo con que otra conversación abrió esta: no lo escribió una persona.
        json!({ "type": "user", "timestamp": "2026-11-12T18:00:00.000Z",
                "message": { "content": "Background (from the session that started this one): armá el servicio con pruebas." } }),
        del_usuario("che, podes revisar por que falla el deploy de staging?", 1),
        de_claude("El despliegue falló porque faltaba la variable de entorno del servicio de correo.", &[]),
        // Pega un renglón de la respuesta para preguntar por él.
        del_usuario(
            "esto no lo entiendo: El despliegue falló porque faltaba la variable de entorno del servicio de correo.",
            2,
        ),
        json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "content": "SALIDA-DE-HERRAMIENTA" }] } }),
        del_usuario("dale, hacelo asi y despues me avisas", 3),
        de_claude("Hecho: agregué la variable y volví a desplegar.", &["Sí, subila", "Todavía no"]),
        // Tocó un botón: el texto lo había escrito Claude.
        del_usuario("Sí, subila", 4),
        del_usuario("no me convence, deberia ser mas simple. vos que opinas?", 5),
        del_usuario("tambien quiero que quede anotado en el readme", 6),
        del_usuario("ok pero cuando lo subimos? tenes una fecha?", 7),
        del_usuario("mandale un mail al cliente con la version nueva, que sea corto", 8),
        del_usuario("y si no anda que hacemos?", 9),
    ]
}

async fn registrar(e: &Estado, sesion: &str, proyecto: &str, transcripcion: &str) {
    let grupo_app = format!("[ACME] - {}", proyecto.to_uppercase());
    let datos = Registro { transcripcion: Some(transcripcion.into()), ..registro("ACME", Some(proyecto), &grupo_app) };
    mcp::contexto(e, &PedidoContexto { sesion: sesion.into(), grupo: None, registro: Some(datos) }).await.unwrap();
}

fn pedido(lugar: &str, sesion: &str) -> PedidoEstilo {
    PedidoEstilo { lugar: Some(lugar.into()), sesion: Some(sesion.into()), ..PedidoEstilo::default() }
}

async fn guardados(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar("select contenido from mensajes_propios order by fecha, id").fetch_all(pool).await.unwrap()
}

/// Los ejemplos que trae una respuesta, en orden.
fn ejemplos(respuesta: &str) -> Vec<&str> {
    respuesta.lines().filter(|l| l.split_once(". [").is_some_and(|(n, _)| n.parse::<u32>().is_ok())).collect()
}

#[tokio::test]
async fn aprende_como_escribe_el_usuario_y_lo_cuenta() {
    let base = comun::Base::nueva().await;
    let e = estado(base.pool.clone()).await;
    let raiz = raiz("aprende");
    let mut conocidos = Ubicaciones::default();

    // Sin mensajes todavía, lo dice.
    let vacio = mcp::estilo(&e, &PedidoEstilo::default()).await.unwrap();
    assert!(vacio.starts_with("Todavía no aprendí cómo escribe el usuario"), "{vacio}");

    registrar(&e, "local_a", "Portal", "abc-1").await;
    escribir(&archivo(&raiz, "abc-1"), &conversacion());
    // Recién escrita, la conversación todavía está en uso: espera.
    assert_eq!(estilo::recorrer(&e, &raiz, &mut conocidos, REPOSO).await.unwrap(), 0);
    assert_eq!(estilo::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 8);
    // Sin nada nuevo no vuelve a leer.
    assert_eq!(estilo::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 0);
    let todos = guardados(&base.pool).await;
    assert_eq!(todos[0], "che, podes revisar por que falla el deploy de staging?");
    assert_eq!(todos[1], "esto no lo entiendo:", "sin lo que pegó de la respuesta");
    assert!(!todos.iter().any(|m| m == "Sí, subila" || m.starts_with("Background")), "{todos:?}");

    let r = mcp::estilo(&e, &pedido("claude", "local_a")).await.unwrap();
    assert!(r.starts_with("Así escribe el usuario en claude · español (8 mensajes suyos, del "), "{r}");
    for esperado in [
        "Empieza con mayúscula: nunca (0 de 8).",
        "Abre las preguntas con «¿»: nunca (0 de 5).",
        "Tildes: las pone nunca (0 de 8)",
        "Trato: de vos",
        "Son pocos mensajes",
        "«y si no anda que hacemos?»",
        "Tengo mensajes suyos de: claude (8).",
    ] {
        assert!(r.contains(esperado), "falta «{esperado}» en:\n{r}");
    }
    for ajeno in ["Sí, subila", "Background", "variable de entorno", "SALIDA-DE-HERRAMIENTA"] {
        assert!(!r.contains(ajeno), "no tendría que estar «{ajeno}» en:\n{r}");
    }
    // Van los seis más nuevos; lo muy corto se mide, pero no va de ejemplo.
    assert_eq!(ejemplos(&r).len(), 6, "{r}");
    assert!(!r.contains("deploy de staging"), "el más viejo no entra: {r}");
    assert!(!r.contains("«esto no lo entiendo:»"), "{r}");

    // Los ejemplos que hablan de lo que se va a escribir van primero, y se puede pedir cuántos.
    let sobre =
        PedidoEstilo { sobre: Some("el deploy de staging".into()), ejemplos: Some(2), ..pedido("claude", "local_a") };
    let r = mcp::estilo(&e, &sobre).await.unwrap();
    let elegidos = ejemplos(&r);
    assert_eq!(elegidos.len(), 2, "{r}");
    assert!(elegidos[0].contains("deploy de staging"), "{r}");

    // De un lugar del que no hay mensajes: avisa y usa lo que tiene.
    let r = mcp::estilo(&e, &pedido("Mail", "local_a")).await.unwrap();
    assert!(r.starts_with("No tengo mensajes suyos de «correo»: lo de abajo sale de claude (8)."), "{r}");
    assert!(r.contains("Así escribe el usuario en todos los lugares juntos · español (8 mensajes suyos"), "{r}");

    // En un idioma en el que no escribió, o que no existe.
    let ingles = PedidoEstilo { idioma: Some("English".into()), ..pedido("claude", "local_a") };
    let r = mcp::estilo(&e, &ingles).await.unwrap();
    assert_eq!(r, "No tengo mensajes suyos en inglés (claude). Tengo de: claude (8).");
    let raro = PedidoEstilo { idioma: Some("klingon".into()), ..pedido("claude", "local_a") };
    assert!(matches!(mcp::estilo(&e, &raro).await, Err(Error::Uso(m)) if m.contains("klingon")));
}

#[tokio::test]
async fn una_conversacion_copiada_no_repite_y_los_ejemplos_dicen_de_que_grupo_son() {
    let base = comun::Base::nueva().await;
    let e = estado(base.pool.clone()).await;
    let raiz = raiz("copia");
    let mut conocidos = Ubicaciones::default();
    registrar(&e, "local_a", "Portal", "abc-1").await;
    escribir(&archivo(&raiz, "abc-1"), &conversacion());
    assert_eq!(estilo::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 8);

    // Otra conversación, de otro proyecto, que arrancó como copia de la primera.
    let mut copia = conversacion();
    copia.push(del_usuario("armame el resumen de la tienda para mañana, que sea cortito", 30));
    registrar(&e, "local_b", "Tienda", "abc-2").await;
    escribir(&archivo(&raiz, "abc-2"), &copia);
    assert_eq!(estilo::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 1, "solo el mensaje nuevo");
    assert_eq!(guardados(&base.pool).await.len(), 9);

    // Desde la Tienda: primero lo que escribió ahí; lo de otro grupo dice de cuál es.
    let r = mcp::estilo(&e, &PedidoEstilo { ejemplos: Some(3), ..pedido("claude", "local_b") }).await.unwrap();
    let elegidos = ejemplos(&r);
    assert_eq!(elegidos.len(), 3, "{r}");
    assert!(elegidos[0].contains("resumen de la tienda") && !elegidos[0].contains("ACME"), "{r}");
    assert!(elegidos[1].contains("· de ACME › Portal]") && elegidos[2].contains("· de ACME › Portal]"), "{r}");
    // Desde el Portal, lo del Portal va sin aclaración y lo de la Tienda la lleva.
    let r = mcp::estilo(&e, &PedidoEstilo { ejemplos: Some(15), ..pedido("claude", "local_a") }).await.unwrap();
    assert!(r.contains("· de ACME › Tienda] «armame el resumen"), "{r}");
    assert!(!r.contains("de ACME › Portal"), "{r}");
}

#[tokio::test]
async fn tapa_los_secretos_y_relee_un_archivo_que_cambio_por_otro() {
    let base = comun::Base::nueva().await;
    let e = estado(base.pool.clone()).await;
    let raiz = raiz("secretos");
    let mut conocidos = Ubicaciones::default();
    registrar(&e, "local_a", "Portal", "abc-1").await;
    let ruta = archivo(&raiz, "abc-1");
    escribir(
        &ruta,
        &[
            del_usuario("probá entrar con password: Abc123def456ghi y decime si anda", 1),
            del_usuario("despues borramos ese usuario de prueba, no hace falta dejarlo", 2),
        ],
    );
    assert_eq!(estilo::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 2);
    let todos = guardados(&base.pool).await;
    assert!(todos[0].starts_with("probá entrar con password: ") && !todos[0].contains("Abc123def456ghi"), "{todos:?}");

    // El archivo pasó a ser otro, más corto: se lee desde el principio.
    escribir(&ruta, &[del_usuario("arrancamos de nuevo con esto", 3)]);
    assert_eq!(estilo::recorrer(&e, &raiz, &mut conocidos, YA).await.unwrap(), 1);
    assert_eq!(guardados(&base.pool).await.len(), 3);
}
