//! WhatsApp como fuente, contra una base de mentira con la misma forma que
//! la de WhatsApp Desktop: qué se lee, qué queda afuera, cuándo toca y cómo
//! se cuenta lo que el usuario le escribe a cada persona.

mod comun;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use comun::estado;
use memoria::config::{Fuentes, WhatsApp};
use memoria::estilo::fuentes::{self, Fuente};
use memoria::estilo::whatsapp::{self, Juntado};
use memoria::mcp::{self, PedidoEstilo};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};

const ANA: i64 = 1;
const EQUIPO: i64 = 2;
const FAMILIA: i64 = 3;
const ESTADOS: i64 = 4;
const SIN_AGENDAR: i64 = 5;
const MARIANA: i64 = 6;
const ANA_PRIMA: i64 = 7;
/// La bandera con que la aplicación marca lo reenviado, y la de un mensaje común.
const REENVIADO_POR_LA_APP: i64 = 0x0100_0180;
const COMUN: i64 = 0x0100_0000;
/// Un momento cualquiera, en segundos desde 2001.
const AHORA: f64 = 816_200_000.0;

/// Una base como la de WhatsApp Desktop, con siete chats.
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
                                  ZFLAGS integer, ZTEXT varchar, ZMESSAGEDATE timestamp, ZCHATSESSION integer);
         insert into ZWACHATSESSION values
           (1, 'Ana Trabajo', '5491100000001@s.whatsapp.net'),
           (2, 'Equipo ACME', '120363000000000001@g.us'),
           (3, 'Familia Pérez', '120363000000000002@g.us'),
           (4, null, 'status@broadcast'),
           (5, '+54 9 11 0000-0002', '5491100000002@s.whatsapp.net'),
           (6, 'Mariana', '100000000000003@lid'),
           (7, 'Ana Prima', '5491100000004@s.whatsapp.net');",
    )
    .await
    .unwrap();
    (archivo, pool)
}

/// Suma un mensaje de texto a un chat; `mio` dice si lo mandó el usuario.
async fn mensaje(whatsapp: &SqlitePool, chat: i64, mio: bool, texto: &str, minuto: i32) {
    con_bandera(whatsapp, chat, mio, texto, minuto, COMUN).await;
}

async fn con_bandera(whatsapp: &SqlitePool, chat: i64, mio: bool, texto: &str, minuto: i32, bandera: i64) {
    sqlx::query(
        "insert into ZWAMESSAGE (ZISFROMME, ZMESSAGETYPE, ZFLAGS, ZTEXT, ZMESSAGEDATE, ZCHATSESSION)
         values (?1, 0, ?2, ?3, ?4, ?5)",
    )
    .bind(i64::from(mio))
    .bind(bandera)
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
    // Copiado y pegado: lo recibió antes, palabra por palabra.
    mensaje(whatsapp, ANA, true, REENVIADO, 12).await;
    // Reenviado desde otro lado: no lo recibió acá, pero la aplicación lo marca.
    con_bandera(whatsapp, ANA, true, "Les comparto la nota que salió hoy en el diario", 12, REENVIADO_POR_LA_APP).await;
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
    sqlx::query(
        "insert into ZWAMESSAGE (ZISFROMME, ZMESSAGETYPE, ZFLAGS, ZTEXT, ZMESSAGEDATE, ZCHATSESSION)
         values (1, 1, 16777216, 'el pie de una foto que mande al grupo', ?1, 2)",
    )
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
    assert_eq!(probado, Juntado { leidos: 12, propios: 10, nuevos: 0, chats: 3 });
    assert_eq!(guardados(&pool).await.len(), 0);

    let juntado = whatsapp::juntar(&pool, &cfg, "Ana", true).await.unwrap();
    assert_eq!(juntado, Juntado { leidos: 12, propios: 10, nuevos: 10, chats: 3 });
    let todos = guardados(&pool).await;
    let textos: Vec<&str> = todos.iter().map(|g| g.4.as_str()).collect();
    assert_eq!(&textos[..6], &A_ANA[..]);
    assert_eq!(textos[6], "alguien pudo revisar lo de ayer? yo no llegue");
    assert_eq!(textos[7], "miren lo que me dijeron:", "sin lo que pegó de otro");
    assert!(textos[8].starts_with("entren con password: ") && !textos[8].contains("Abc123def456ghi"), "{}", textos[8]);
    assert_eq!(textos[9], "ok, mañana te confirmo el horario");
    for ajeno in [
        "Perfecto, gracias",
        "Les paso el link",
        "la nota que salió",
        "no me esperen",
        "un estado",
        "el pie de una foto",
    ] {
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
    // Las otras pruebas de este archivo corren a la vez, y cada una tiene su
    // copia mientras lee: se espera a que no quede ninguna.
    let mut sueltas = copias_sueltas();
    for _ in 0..100 {
        if sueltas == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        sueltas = copias_sueltas();
    }
    assert_eq!(sueltas, 0);
    let ahora = std::fs::read(&archivo).unwrap();
    assert!(ahora.len() >= antes.len());
}

#[tokio::test]
async fn los_mensajes_a_alguien_se_muestran_solo_si_se_pregunta_por_esa_persona() {
    let base = comun::Base::nueva().await;
    let e = estado(base.pool.clone()).await;
    let (archivo, de_whatsapp) = base_de_whatsapp("ejemplos").await;
    cargar(&de_whatsapp).await;
    mensaje(&de_whatsapp, MARIANA, true, "mari, te dejo las llaves abajo del felpudo de la entrada", 20).await;
    whatsapp::juntar(&e.pool, &ajustes(&archivo), "Ana", true).await.unwrap();

    let pedir = |para: Option<&str>| PedidoEstilo {
        lugar: Some("WhatsApp".into()),
        para: para.map(Into::into),
        ejemplos: Some(15),
        ..PedidoEstilo::default()
    };
    // Sin decir a quién: lo medido sí, los mensajes no.
    let r = mcp::estilo(&e, &pedir(None)).await.unwrap();
    assert!(r.starts_with("Así escribe el usuario en whatsapp · español (11 mensajes suyos"), "{r}");
    assert!(r.contains("No muestro mensajes suyos de ejemplo") && r.contains("Decí a quién le va a escribir"), "{r}");
    assert!(!r.contains("te lo mando mañana") && !r.contains("felpudo"), "{r}");

    // A una persona: solo lo que le escribió a ella. «Ana» no es «Mariana».
    let r = mcp::estilo(&e, &pedir(Some("ana"))).await.unwrap();
    assert!(r.contains("· a Ana Trabajo] «dale, te lo mando mañana a primera hora»"), "{r}");
    assert!(!r.contains("Equipo ACME") && !r.contains("alguien pudo revisar") && !r.contains("felpudo"), "{r}");
    assert!(r.contains("A «Ana Trabajo» le escribió poco (6 mensajes)"), "{r}");
    // A un grupo: lo del grupo, sin la clave tapada ni lo que presenta algo pegado.
    let r = mcp::estilo(&e, &pedir(Some("Equipo"))).await.unwrap();
    assert!(r.contains("· a Equipo ACME] «alguien pudo revisar lo de ayer? yo no llegue»"), "{r}");
    assert!(!r.contains("password") && !r.contains("Ana Trabajo"), "{r}");
    // A alguien a quien nunca le escribió: lo dice y no muestra nada de otros.
    let r = mcp::estilo(&e, &pedir(Some("Marta"))).await.unwrap();
    assert!(r.contains("No encontré a «Marta» entre las personas y los grupos"), "{r}");
    assert!(r.contains("No muestro mensajes suyos de ejemplo") && !r.contains("· a "), "{r}");

    // Si el nombre coincide con dos chats no elige: los nombra y no muestra nada de ninguno.
    mensaje(&de_whatsapp, ANA_PRIMA, true, "prima, el domingo comemos en lo de la abuela?", 21).await;
    whatsapp::juntar(&e.pool, &ajustes(&archivo), "Ana", true).await.unwrap();
    let r = mcp::estilo(&e, &pedir(Some("Ana"))).await.unwrap();
    assert!(r.contains("«Ana» coincide con varios chats suyos: «Ana Trabajo», «Ana Prima»."), "{r}");
    assert!(!r.contains("· a ") && !r.contains("abuela") && !r.contains("te lo mando"), "{r}");
    let r = mcp::estilo(&e, &pedir(Some("Ana Prima"))).await.unwrap();
    assert!(r.contains("· a Ana Prima] «prima, el domingo comemos en lo de la abuela?»"), "{r}");
    assert!(!r.contains("Ana Trabajo"), "{r}");
    de_whatsapp.close().await;
}

/// Mensajes de sobra a una persona, escritos de otra forma que al resto.
const A_LUIS: [&str; 6] = [
    "Estimado Luis, ¿podrías confirmarme la reunión del jueves?",
    "Muchas gracias por la respuesta. Quedo atento a tus comentarios.",
    "Buenas tardes, te envío el resumen de lo conversado en la reunión.",
    "¿Te parece bien si lo revisamos la semana próxima?",
    "Perfecto, entonces avanzamos con la propuesta que te mandé.",
    "Te confirmo que mañana enviamos la versión final. Saludos.",
];

#[tokio::test]
async fn con_bastantes_mensajes_a_una_persona_mide_como_le_escribe_a_ella() {
    let base = comun::Base::nueva().await;
    let e = estado(base.pool.clone()).await;
    let (archivo, de_whatsapp) = base_de_whatsapp("persona").await;
    cargar(&de_whatsapp).await;
    sqlx::query("insert into ZWACHATSESSION values (8, 'Luis Cliente', '5491100000005@s.whatsapp.net')")
        .execute(&de_whatsapp)
        .await
        .unwrap();
    // Seis charlas de seis mensajes, una por día: en cada una saluda al empezar.
    for dia in 0..6 {
        for (i, texto) in A_LUIS.iter().enumerate() {
            let texto = format!("{texto} ({dia}.{i})");
            mensaje(&de_whatsapp, 8, true, &texto, 100 + dia * 24 * 60 + i32::try_from(i).unwrap()).await;
        }
    }
    whatsapp::juntar(&e.pool, &ajustes(&archivo), "Ana", true).await.unwrap();

    let pedir = |para: &str| PedidoEstilo {
        lugar: Some("whatsapp".into()),
        para: Some(para.into()),
        ..PedidoEstilo::default()
    };
    let r = mcp::estilo(&e, &pedir("Luis")).await.unwrap();
    assert!(
        r.starts_with("Así le escribe el usuario a «Luis Cliente» en whatsapp · español (36 mensajes suyos"),
        "{r}"
    );
    // A Luis le escribe con mayúsculas y abriendo las preguntas; al resto, no.
    assert!(r.contains("Empieza con mayúscula: siempre (36 de 36)."), "{r}");
    assert!(r.contains("Abre las preguntas con «¿»: siempre (12 de 12)."), "{r}");
    // Seis charlas: en todas empezó saludando, aunque sean seis mensajes de treinta y seis.
    assert!(r.contains("Saluda al empezar una charla: siempre (6 de 6)."), "{r}");
    assert!(r.contains("· a Luis Cliente] «"), "{r}");
    assert!(!r.contains("le escribió poco") && !r.contains("Ana Trabajo"), "{r}");

    // En general (sin decir a quién) lo de Luis pesa, pero no es lo único.
    let r = mcp::estilo(&e, &PedidoEstilo { lugar: Some("whatsapp".into()), ..PedidoEstilo::default() }).await.unwrap();
    assert!(r.starts_with("Así escribe el usuario en whatsapp · español (46 mensajes suyos"), "{r}");
    assert!(r.contains("Empieza con mayúscula: casi siempre (36 de 46)."), "{r}");
    de_whatsapp.close().await;
}

#[tokio::test]
async fn se_lee_cuando_toca_y_no_se_insiste_si_fallo() {
    let base = comun::Base::nueva().await;
    let mut e = estado(base.pool.clone()).await;
    let pool = base.pool.clone();
    let (archivo, de_whatsapp) = base_de_whatsapp("toca").await;
    cargar(&de_whatsapp).await;

    // Sin ninguna fuente encendida no hay nada que leer ni que avisar.
    assert!(!fuentes::toca(&pool, &e.fuentes).await.unwrap());
    let (bien, resumen) = fuentes::juntar(&pool, &e.fuentes, "Ana").await.unwrap();
    assert!(bien && resumen.starts_with("No hay ninguna fuente encendida"), "{resumen}");
    assert_eq!(fuentes::ultima(&pool, Fuente::WhatsApp).await.unwrap(), None);
    assert_eq!(fuentes::aviso(&e).await.unwrap(), None);

    let encendidas = Fuentes { whatsapp: Some(ajustes(&archivo)), slack: None };
    e.fuentes = Arc::new(encendidas.clone());
    assert!(fuentes::toca(&pool, &encendidas).await.unwrap());
    // Probar no guarda ni cuenta como juntada.
    let (bien, resumen) = fuentes::probar(&pool, &encendidas, "Ana").await;
    assert!(bien && resumen.starts_with("WhatsApp: entrarían 10 mensajes tuyos de 3 chats"), "{resumen}");
    assert!(fuentes::toca(&pool, &encendidas).await.unwrap());

    let (bien, resumen) = fuentes::juntar(&pool, &encendidas, "Ana").await.unwrap();
    assert!(bien && resumen.starts_with("WhatsApp: 10 mensajes nuevos"), "{resumen}");
    // Recién leído no toca de nuevo, y no hay nada que avisar.
    assert!(!fuentes::toca(&pool, &encendidas).await.unwrap());
    assert_eq!(fuentes::aviso(&e).await.unwrap(), None);
    // A mano se puede leer cuando se quiera.
    let (bien, resumen) = fuentes::juntar(&pool, &encendidas, "Ana").await.unwrap();
    assert!(bien && resumen.starts_with("WhatsApp: 0 mensajes nuevos"), "{resumen}");

    // Si no se puede leer, queda anotado, no se insiste enseguida y quien pregunta cómo escribe se entera.
    let rota = Fuentes {
        whatsapp: Some(WhatsApp { base: archivo.with_file_name("no-existe.sqlite"), sin: vec![] }),
        slack: None,
    };
    let (bien, resumen) = fuentes::juntar(&pool, &rota, "Ana").await.unwrap();
    assert!(!bien && resumen.starts_with("WhatsApp: no se pudo leer. no pude leer la base de WhatsApp"), "{resumen}");
    let ultima = fuentes::ultima(&pool, Fuente::WhatsApp).await.unwrap().unwrap();
    assert!(!ultima.bien && ultima.resumen == resumen);
    assert!(!fuentes::toca(&pool, &rota).await.unwrap());
    let r = mcp::estilo(&e, &PedidoEstilo { lugar: Some("whatsapp".into()), ..PedidoEstilo::default() }).await.unwrap();
    assert!(r.contains("WhatsApp está encendido, pero la última lectura (") && r.contains("no anduvo"), "{r}");

    // Mientras el servicio espera a que el usuario le conteste a macOS, también.
    let hace_cinco_minutos = chrono::Utc::now().timestamp() - 5 * 60;
    e.leyendo_whatsapp.store(hace_cinco_minutos, std::sync::atomic::Ordering::Relaxed);
    let aviso = fuentes::aviso(&e).await.unwrap().unwrap();
    assert!(aviso.contains("la lectura espera hace 5 min") && aviso.contains("«Permitir»"), "{aviso}");
    // Recién empezada no se avisa nada: lo normal es que termine en segundos.
    e.leyendo_whatsapp.store(chrono::Utc::now().timestamp(), std::sync::atomic::Ordering::Relaxed);
    assert_eq!(fuentes::aviso(&e).await.unwrap(), None);
    de_whatsapp.close().await;
}
