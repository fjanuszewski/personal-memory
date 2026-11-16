//! WhatsApp como fuente, contra una base de mentira con la misma forma que
//! la de WhatsApp Desktop: qué se lee, qué queda afuera y cuándo toca.

mod comun;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use comun::estado;
use memoria::config::{Fuentes, WhatsApp};
use memoria::estilo::fuentes::{self, Modo};
use memoria::estilo::whatsapp::{self, Juntado};
use memoria::mcp::{self, PedidoEstado, PedidoEstilo};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};

const ANA: i64 = 1;
const EQUIPO: i64 = 2;
const FAMILIA: i64 = 3;
const ESTADOS: i64 = 4;
const SIN_AGENDAR: i64 = 5;
/// Un momento cualquiera, en segundos desde 2001.
const AHORA: f64 = 816_200_000.0;

/// Una base como la de WhatsApp Desktop, con cinco chats.
async fn base_de_whatsapp(nombre: &str) -> (PathBuf, SqlitePool) {
    let carpeta = std::env::temp_dir().join(format!("memoria-prueba-wa-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&carpeta);
    std::fs::create_dir_all(&carpeta).unwrap();
    let archivo = carpeta.join("ChatStorage.sqlite");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&archivo).create_if_missing(true))
        .await
        .unwrap();
    pool.execute(
        "create table ZWACHATSESSION (Z_PK integer primary key, ZPARTNERNAME varchar, ZCONTACTJID varchar);
         create table ZWAMESSAGE (Z_PK integer primary key autoincrement, ZISFROMME integer, ZMESSAGETYPE integer,
                                  ZTEXT varchar, ZMESSAGEDATE timestamp, ZCHATSESSION integer);
         insert into ZWACHATSESSION values
           (1, 'Ana Trabajo', '5491100000001@s.whatsapp.net'),
           (2, 'Equipo ACME', '120363000000000001@g.us'),
           (3, 'Familia Pérez', '120363000000000002@g.us'),
           (4, null, 'status@broadcast'),
           (5, '+54 9 11 0000-0002', '5491100000002@s.whatsapp.net');",
    )
    .await
    .unwrap();
    (archivo, pool)
}

/// Suma un mensaje de texto a un chat; `mio` dice si lo mandó el usuario.
async fn mensaje(whatsapp: &SqlitePool, chat: i64, mio: bool, texto: &str, minuto: i32) {
    sqlx::query("insert into ZWAMESSAGE (ZISFROMME, ZMESSAGETYPE, ZTEXT, ZMESSAGEDATE, ZCHATSESSION) values (?1, 0, ?2, ?3, ?4)")
        .bind(i64::from(mio))
        .bind(texto)
        .bind(AHORA + f64::from(minuto) * 60.0)
        .bind(chat)
        .execute(whatsapp)
        .await
        .unwrap();
}

const A_ANA: [&str; 6] = [
    "dale, te lo mando mañana a primera hora",
    "che pudiste ver lo que te pase ayer?",
    "no llego para las 10, arranquen sin mi",
    "ok despues lo vemos con mas tiempo",
    "tenes el link de la reunion? no lo encuentro",
    "mañana no puedo, lo pasamos para el jueves?",
];
const REENVIADO: &str = "Hola! Les paso el link de la reunión de mañana a las 10";

async fn cargar(whatsapp: &SqlitePool) {
    for (i, texto) in A_ANA.iter().enumerate() {
        mensaje(whatsapp, ANA, true, texto, i32::try_from(i).unwrap()).await;
    }
    // Lo que escriben los demás no entra nunca.
    mensaje(whatsapp, ANA, false, "Perfecto, gracias! Cualquier cosa te aviso", 10).await;
    mensaje(whatsapp, EQUIPO, false, REENVIADO, 11).await;
    // Reenviado: lo recibió antes, palabra por palabra.
    mensaje(whatsapp, ANA, true, REENVIADO, 12).await;
    mensaje(whatsapp, EQUIPO, true, "alguien pudo revisar lo de ayer? yo no llegue", 13).await;
    // Pegó lo que le escribió otro, y una clave.
    mensaje(whatsapp, EQUIPO, true, "miren lo que me dijeron:\n[22/10, 8:16] Pedro: llegamos el lunes", 14).await;
    mensaje(whatsapp, EQUIPO, true, "entren con password: Abc123def456ghi y me avisan", 15).await;
    // Chats que no se leen: el que el usuario dejó afuera y los estados.
    mensaje(whatsapp, FAMILIA, true, "llego tarde, no me esperen para comer", 16).await;
    mensaje(whatsapp, ESTADOS, true, "un estado que publique para todos mis contactos", 17).await;
    // Un contacto sin agendar: el mensaje vale, el número no se guarda.
    mensaje(whatsapp, SIN_AGENDAR, true, "ok, mañana te confirmo el horario", 18).await;
    // Lo que no es texto no se lee: una foto con su pie.
    sqlx::query("insert into ZWAMESSAGE (ZISFROMME, ZMESSAGETYPE, ZTEXT, ZMESSAGEDATE, ZCHATSESSION) values (1, 1, 'el pie de una foto que mande al grupo', ?1, 2)")
        .bind(AHORA + 19.0 * 60.0)
        .execute(whatsapp)
        .await
        .unwrap();
}

fn ajustes(archivo: &Path) -> WhatsApp {
    WhatsApp { base: archivo.to_owned(), sin: vec!["familia".into()] }
}

type Guardado = (String, String, Option<String>, Option<String>, String);

async fn guardados(pool: &SqlitePool) -> Vec<Guardado> {
    sqlx::query_as("select fuente, lugar, conversacion, para, contenido from mensajes_propios order by fecha, id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Las copias temporales que hayan quedado de este programa.
fn copias_sueltas() -> usize {
    let prefijo = format!("memoria-whatsapp-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefijo))
        .count()
}

#[tokio::test]
async fn lee_solo_lo_que_escribio_el_usuario_y_no_toca_la_base_de_whatsapp() {
    let base = comun::Base::nueva().await;
    let pool = base.pool.clone();
    let (archivo, de_whatsapp) = base_de_whatsapp("lee").await;
    cargar(&de_whatsapp).await;
    let cfg = ajustes(&archivo);
    let antes = std::fs::read(&archivo).unwrap();

    // Probando: cuenta y no guarda.
    let probado = whatsapp::juntar(&pool, &cfg, "Ana", false).await.unwrap();
    assert_eq!(probado, Juntado { leidos: 11, propios: 10, nuevos: 0, chats: 3 });
    assert_eq!(guardados(&pool).await.len(), 0);

    let juntado = whatsapp::juntar(&pool, &cfg, "Ana", true).await.unwrap();
    assert_eq!(juntado, Juntado { leidos: 11, propios: 10, nuevos: 10, chats: 3 });
    let todos = guardados(&pool).await;
    let textos: Vec<&str> = todos.iter().map(|g| g.4.as_str()).collect();
    assert_eq!(&textos[..6], &A_ANA[..]);
    assert_eq!(textos[6], "alguien pudo revisar lo de ayer? yo no llegue");
    assert_eq!(textos[7], "miren lo que me dijeron:", "sin lo que pegó de otro");
    assert!(textos[8].starts_with("entren con password: ") && !textos[8].contains("Abc123def456ghi"), "{}", textos[8]);
    assert_eq!(textos[9], "ok, mañana te confirmo el horario");
    for ajeno in ["Perfecto, gracias", "Les paso el link", "no me esperen", "un estado", "el pie de una foto"] {
        assert!(!textos.iter().any(|t| t.contains(ajeno)), "no tendría que estar «{ajeno}»: {textos:?}");
    }
    // De cada chat queda el nombre y una marca: nunca el número.
    for (fuente, lugar, conversacion, para, _) in &todos {
        assert_eq!((fuente.as_str(), lugar.as_str()), ("whatsapp", "whatsapp"));
        let conversacion = conversacion.as_deref().unwrap();
        assert!(conversacion.starts_with("wa:") && !conversacion.contains("5491100"), "{conversacion}");
        assert!(!para.as_deref().unwrap_or_default().contains("0000"), "{para:?}");
    }
    assert_eq!(todos[0].3.as_deref(), Some("Ana Trabajo"));
    assert_eq!(todos[6].3.as_deref(), Some("Equipo ACME"));
    assert_eq!(todos[9].3, None, "el contacto sin agendar figura con su número");

    // Sin nada nuevo no guarda nada; lo que se manda después se lee desde ahí.
    assert_eq!(whatsapp::juntar(&pool, &cfg, "Ana", true).await.unwrap(), Juntado::default());
    mensaje(&de_whatsapp, ANA, true, "listo, ya te lo mande por mail", 30).await;
    let despues = whatsapp::juntar(&pool, &cfg, "Ana", true).await.unwrap();
    assert_eq!(despues, Juntado { leidos: 1, propios: 1, nuevos: 1, chats: 1 });

    // La base de la aplicación quedó como estaba, y no quedó ninguna copia dando vueltas.
    de_whatsapp.close().await;
    assert_eq!(copias_sueltas(), 0);
    let ahora = std::fs::read(&archivo).unwrap();
    assert!(ahora.len() >= antes.len());
}

#[tokio::test]
async fn los_mensajes_a_alguien_se_muestran_solo_si_se_pregunta_por_esa_persona() {
    let base = comun::Base::nueva().await;
    let e = estado(base.pool.clone()).await;
    let (archivo, de_whatsapp) = base_de_whatsapp("ejemplos").await;
    cargar(&de_whatsapp).await;
    whatsapp::juntar(&e.pool, &ajustes(&archivo), "Ana", true).await.unwrap();

    let pedir = |para: Option<&str>| PedidoEstilo {
        lugar: Some("WhatsApp".into()),
        para: para.map(Into::into),
        ejemplos: Some(15),
        ..PedidoEstilo::default()
    };
    // Sin decir a quién: lo medido sí, los mensajes no.
    let r = mcp::estilo(&e, &pedir(None)).await.unwrap();
    assert!(r.starts_with("Así escribe el usuario en whatsapp · español (10 mensajes suyos"), "{r}");
    assert!(r.contains("No muestro mensajes suyos de ejemplo"), "{r}");
    assert!(!r.contains('«') || !r.contains("te lo mando mañana"), "{r}");

    // A una persona: solo lo que le escribió a ella.
    let r = mcp::estilo(&e, &pedir(Some("ana"))).await.unwrap();
    assert!(r.contains("· a Ana Trabajo] «dale, te lo mando mañana a primera hora»"), "{r}");
    assert!(!r.contains("Equipo ACME") && !r.contains("alguien pudo revisar"), "{r}");
    // A un grupo: lo del grupo, sin la clave tapada ni lo que presenta algo pegado.
    let r = mcp::estilo(&e, &pedir(Some("Equipo"))).await.unwrap();
    assert!(r.contains("· a Equipo ACME] «alguien pudo revisar lo de ayer? yo no llegue»"), "{r}");
    assert!(!r.contains("password") && !r.contains("Ana Trabajo"), "{r}");
    // A alguien a quien nunca le escribió: lo dice y no muestra nada de otros.
    let r = mcp::estilo(&e, &pedir(Some("Marta"))).await.unwrap();
    assert!(r.contains("No muestro mensajes suyos de ejemplo"), "{r}");
    de_whatsapp.close().await;
}

#[tokio::test]
async fn el_servicio_avisa_cuando_toca_y_no_insiste_si_fallo() {
    let base = comun::Base::nueva().await;
    let mut e = estado(base.pool.clone()).await;
    let pool = base.pool.clone();
    let (archivo, de_whatsapp) = base_de_whatsapp("toca").await;
    cargar(&de_whatsapp).await;
    comun::empezar(&e, "local_a", "ACME", Some("Portal")).await;
    let al_mod = |e: &memoria::estado::Estado| {
        let e = e.clone();
        async move {
            let pedido = PedidoEstado { sesion: Some("local_a".into()), ..PedidoEstado::default() };
            let texto = mcp::mejora_estado(&e, &pedido).await.unwrap();
            serde_json::from_str::<serde_json::Value>(&texto).unwrap()["juntar"].clone()
        }
    };

    // Sin ninguna fuente encendida no se pide nada.
    assert!(!fuentes::toca(&pool, &e.fuentes).await.unwrap());
    assert!(al_mod(&e).await.is_null());
    let (bien, resumen) = fuentes::juntar(&pool, &e.fuentes, "Ana", Modo::Ya).await.unwrap();
    assert!(bien && resumen.starts_with("No hay ninguna fuente encendida"), "{resumen}");

    // Encendida: toca, y el mod recibe el programa que tiene que correr.
    let encendidas = Fuentes { whatsapp: Some(ajustes(&archivo)) };
    e.fuentes = Arc::new(encendidas.clone());
    assert!(fuentes::toca(&pool, &encendidas).await.unwrap());
    let programa = al_mod(&e).await;
    assert!(programa.as_str().is_some_and(|p| p.starts_with('/')), "{programa}");

    // Probar no guarda ni cuenta como juntada.
    let (bien, resumen) = fuentes::juntar(&pool, &encendidas, "Ana", Modo::Probar).await.unwrap();
    assert!(bien && resumen.starts_with("WhatsApp: entrarían 10 mensajes tuyos de 3 chats"), "{resumen}");
    assert!(fuentes::toca(&pool, &encendidas).await.unwrap());

    let (bien, resumen) = fuentes::juntar(&pool, &encendidas, "Ana", Modo::SiToca).await.unwrap();
    assert!(bien && resumen.starts_with("WhatsApp: 10 mensajes nuevos"), "{resumen}");
    // Recién juntado, no toca: ni el servicio lo pide ni el programa lo hace de nuevo.
    assert!(!fuentes::toca(&pool, &encendidas).await.unwrap());
    assert!(al_mod(&e).await.is_null());
    let (_, resumen) = fuentes::juntar(&pool, &encendidas, "Ana", Modo::SiToca).await.unwrap();
    assert_eq!(resumen, "Todavía no toca juntar de nuevo.");
    // Salvo que se pida ya.
    let (bien, resumen) = fuentes::juntar(&pool, &encendidas, "Ana", Modo::Ya).await.unwrap();
    assert!(bien && resumen.starts_with("WhatsApp: 0 mensajes nuevos"), "{resumen}");

    // Si no se puede leer, queda anotado y no se insiste enseguida.
    let rota = Fuentes { whatsapp: Some(WhatsApp { base: archivo.with_file_name("no-existe.sqlite"), sin: vec![] }) };
    let (bien, resumen) = fuentes::juntar(&pool, &rota, "Ana", Modo::Ya).await.unwrap();
    assert!(!bien && resumen.starts_with("WhatsApp: no se pudo leer. no pude leer la base de WhatsApp"), "{resumen}");
    let ultima = fuentes::ultima(&pool).await.unwrap().unwrap();
    assert!(!ultima.bien && ultima.resumen == resumen);
    assert!(!fuentes::toca(&pool, &rota).await.unwrap());
    de_whatsapp.close().await;
}
