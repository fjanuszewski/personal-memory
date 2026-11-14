//! Alta de notas: validación, filtro de secretos, repetidas (iguales o que
//! dicen lo mismo con otras palabras) y reemplazos.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::{Captures, Regex};
use sqlx::{SqliteConnection, SqlitePool};

use crate::base;
use crate::error::{Resultado, uso};
use crate::estado::{Estado, Umbrales};
use crate::grupos::Grupo;
use crate::huellas::texto_de_nota;
use crate::indice::{self, Clase, Dato, Indice};
use crate::modelo::TipoNota;
use crate::texto;

const LARGO_TITULO: usize = 200;
const LARGO_CUERPO: usize = 4000;
/// Con cuántas notas cercanas se compara la nueva.
const CANDIDATAS: usize = 3;
const TAPADO: &str = "[secreto quitado]";

#[derive(Clone, Debug)]
pub struct NuevaNota {
    pub tipo: TipoNota,
    pub titulo: String,
    pub cuerpo: String,
    pub repos: Vec<String>,
    pub etiquetas: Vec<String>,
    /// La nota que esta deja como reemplazada.
    pub reemplaza: Option<i64>,
    /// La conversación de donde sale.
    pub origen: Option<String>,
}

/// Otra nota vigente del grupo que dice algo cercano.
#[derive(Clone, Debug, PartialEq)]
pub struct Parecida {
    pub id: i64,
    pub titulo: String,
    pub parecido: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Guardado {
    Nueva {
        id: i64,
        reemplazo: Option<i64>,
        parecida: Option<Parecida>,
    },
    /// Ya había una vigente igual: se le sumó el origen.
    Repetida {
        id: i64,
    },
    /// Ya había una vigente que decía lo mismo con otras palabras: quedó con
    /// el texto nuevo y se le sumó el origen.
    Unida {
        id: i64,
        titulo_anterior: String,
    },
}

/// Patrones de claves y tokens conocidos, con el nombre que se le muestra a
/// Claude cuando aparece uno.
static SECRETOS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("una clave de acceso de AWS", r"\b(AKIA|ASIA)[0-9A-Z]{16}\b"),
        ("una clave privada", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
        ("un token de GitHub", r"\b(gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})"),
        ("un token de Slack", r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
        ("una clave de Anthropic", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
        ("una clave de API", r"\bsk-(proj-)?[A-Za-z0-9_-]{32,}"),
        ("una clave de Google", r"\bAIza[0-9A-Za-z_-]{35}"),
        ("una clave de Stripe", r"\b[rs]k_(live|test)_[0-9A-Za-z]{16,}"),
        ("un token JWT", r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"),
        ("una contraseña dentro de una dirección", r"[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]{3,}@"),
    ]
    .into_iter()
    .map(|(nombre, patron)| (nombre, Regex::new(patron).expect("patrón válido")))
    .collect()
});

/// «password: X», «token = X»… con un valor que parece una clave de verdad
/// (largo, con letras y números), no una explicación.
static ASIGNACION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(password|passwd|pwd|contraseña|clave|secret|secreto|token|api[_-]?key)\b\s*[:=]\s*['"]?([^\s'"]{12,})"#,
    )
    .expect("patrón válido")
});

fn parece_clave(valor: &str) -> bool {
    valor.chars().any(|c| c.is_ascii_digit()) && valor.chars().any(char::is_alphabetic)
}

/// Qué tipo de secreto aparece en el texto, si aparece alguno.
#[must_use]
pub fn buscar_secreto(texto: &str) -> Option<&'static str> {
    if let Some((nombre, _)) = SECRETOS.iter().find(|(_, re)| re.is_match(texto)) {
        return Some(nombre);
    }
    ASIGNACION
        .captures_iter(texto)
        .filter_map(|c| c.get(2))
        .any(|valor| parece_clave(valor.as_str()))
        .then_some("una contraseña o un token")
}

/// El mismo texto con las claves y los tokens tapados. Es para los pedazos de
/// conversaciones: una nota con un secreto se rechaza, pero de una
/// conversación se guarda el resto.
#[must_use]
pub fn tapar_secretos(texto: &str) -> Cow<'_, str> {
    if buscar_secreto(texto).is_none() {
        return Cow::Borrowed(texto);
    }
    let mut salida = texto.to_owned();
    for (_, re) in SECRETOS.iter() {
        if re.is_match(&salida) {
            salida = re.replace_all(&salida, TAPADO).into_owned();
        }
    }
    let salida = ASIGNACION.replace_all(&salida, |c: &Captures<'_>| {
        if parece_clave(&c[2]) { c[0].replace(&c[2], TAPADO) } else { c[0].to_owned() }
    });
    Cow::Owned(salida.into_owned())
}

const DIAS_Y_MESES: [&str; 19] = [
    "lunes",
    "martes",
    "miercoles",
    "jueves",
    "viernes",
    "sabado",
    "domingo",
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
];

fn sin_acentos(texto: &str) -> String {
    texto
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            otro => otro,
        })
        .collect()
}

fn es_dato_duro(palabra: &str, empieza_oracion: bool) -> bool {
    if palabra.is_empty() {
        return false;
    }
    if palabra.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    if palabra.chars().count() < 2 {
        return false;
    }
    palabra.chars().skip(1).any(char::is_uppercase)
        || (!empieza_oracion && palabra.chars().next().is_some_and(char::is_uppercase))
        || DIAS_Y_MESES.contains(&sin_acentos(&palabra.to_lowercase()).as_str())
}

/// Los datos de un texto que no admiten «más o menos»: números, fechas,
/// identificadores, días, meses y nombres propios. Dos notas pueden decir
/// casi lo mismo y ser hechos distintos si cambia uno de estos («la factura
/// de octubre» y «la de noviembre»), así que no se unen.
#[must_use]
pub fn datos_duros(texto: &str) -> BTreeSet<String> {
    let mut datos = BTreeSet::new();
    for linea in texto.lines() {
        // La primera palabra de una oración lleva mayúscula sin ser un nombre.
        let mut empieza_oracion = true;
        for crudo in linea.split_whitespace() {
            let palabra = crudo.trim_matches(|c: char| !c.is_alphanumeric());
            if es_dato_duro(palabra, empieza_oracion) {
                datos.insert(sin_acentos(&palabra.to_lowercase()));
            }
            empieza_oracion = crudo.ends_with(['.', ':', '!', '?']);
        }
    }
    datos
}

/// Si los datos duros de una nota contradicen a los de la otra: cada una
/// tiene alguno que la otra no. (Que una tenga datos de más no es choque.)
#[must_use]
pub fn chocan(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    !a.is_subset(b) && !b.is_subset(a)
}

fn una_linea(texto: &str) -> String {
    texto.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn lista(valores: &[String], minusculas: bool) -> Vec<String> {
    let mut salida: Vec<String> = Vec::new();
    for v in valores.iter().map(|v| v.trim()).filter(|v| !v.is_empty()) {
        let v = if minusculas { v.to_lowercase() } else { v.to_owned() };
        if !salida.contains(&v) {
            salida.push(v);
        }
    }
    salida
}

/// Una nota vigente del grupo, con qué tan cerca está de la nueva (1 = igual).
struct Candidata {
    id: i64,
    titulo: String,
    cuerpo: String,
    tipo: String,
    creada: DateTime<Utc>,
    parecido: f64,
}

enum Decision {
    Unir(usize),
    Avisar(usize),
    Nada,
}

/// Qué hacer con la nota nueva frente a las vigentes más cercanas (ordenadas
/// de la más parecida a la menos): unirla a una, avisar que se parece o nada.
fn decidir(candidatas: &[Candidata], tipo: &str, texto: &str, umbrales: &Umbrales) -> Decision {
    let datos = datos_duros(texto);
    let une = candidatas.iter().position(|c| {
        c.parecido >= umbrales.union
            && c.tipo == tipo
            && !chocan(&datos, &datos_duros(&format!("{}\n{}", c.titulo, c.cuerpo)))
    });
    if let Some(i) = une {
        return Decision::Unir(i);
    }
    match candidatas.first() {
        Some(c) if c.parecido >= umbrales.aviso => Decision::Avisar(0),
        _ => Decision::Nada,
    }
}

/// Una nota ya revisada: título en una línea, largos válidos y sin secretos.
#[derive(Clone, Debug)]
pub struct NotaLista {
    tipo: TipoNota,
    titulo: String,
    cuerpo: String,
    repos: Vec<String>,
    etiquetas: Vec<String>,
    origenes: Vec<String>,
    reemplaza: Option<i64>,
}

impl NotaLista {
    /// El texto que se convierte en huella.
    #[must_use]
    pub fn texto(&self) -> String {
        texto_de_nota(&self.titulo, &self.cuerpo)
    }
}

/// Revisa la nota antes de guardarla: que tenga título y cuerpo, que no sea
/// larga y que no traiga claves ni tokens.
pub fn revisar(nota: &NuevaNota) -> Resultado<NotaLista> {
    let titulo = una_linea(&nota.titulo);
    let cuerpo = nota.cuerpo.trim().to_owned();
    if titulo.is_empty() || cuerpo.is_empty() {
        return uso("La nota necesita título y cuerpo.");
    }
    if titulo.chars().count() > LARGO_TITULO {
        return uso(format!("El título es largo: dejalo en una línea de hasta {LARGO_TITULO} caracteres."));
    }
    if cuerpo.chars().count() > LARGO_CUERPO {
        return uso(format!(
            "El cuerpo pasa de {LARGO_CUERPO} caracteres: resumilo (qué, por qué, cómo) y enlazá el repo o la conversación para el resto."
        ));
    }
    let repos = lista(&nota.repos, false);
    let etiquetas = lista(&nota.etiquetas, true);
    let todo = format!("{titulo}\n{cuerpo}\n{}\n{}", repos.join(" "), etiquetas.join(" "));
    if let Some(secreto) = buscar_secreto(&todo) {
        return uso(format!(
            "No la guardé: parece tener {secreto}. Sacalo y volvé a guardarla; en la memoria nunca van claves, contraseñas ni tokens."
        ));
    }
    let origenes: Vec<String> = nota.origen.iter().map(|o| o.trim().to_owned()).filter(|o| !o.is_empty()).collect();
    Ok(NotaLista { tipo: nota.tipo, titulo, cuerpo, repos, etiquetas, origenes, reemplaza: nota.reemplaza })
}

/// Las vigentes del grupo más cercanas a una huella. Se compara contra todas
/// las del grupo, sin atajos, para no saltear ninguna. `salvo` es la que la
/// nota nueva reemplaza: se parecen a propósito.
async fn candidatas(
    conn: &mut SqliteConnection,
    indice: &Indice,
    grupo: i64,
    huella: &[f32],
    salvo: Option<i64>,
) -> Resultado<Vec<Candidata>> {
    let vigentes: Vec<i64> = sqlx::query_scalar(
        "select id from notas
         where grupo_id = ?1 and estado = 'vigente' and huella is not null and id is not ?2",
    )
    .bind(grupo)
    .bind(salvo)
    .fetch_all(&mut *conn)
    .await?;
    let vigentes: HashSet<i64> = vigentes.into_iter().collect();
    let cercanas = indice.cercanas(Clase::Notas, huella, CANDIDATAS, &|d| vigentes.contains(&d.id));
    if cercanas.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = cercanas.iter().map(|(d, _)| d.id).collect();
    let filas: Vec<(i64, String, String, String, DateTime<Utc>)> = sqlx::query_as(
        "select id, titulo, cuerpo, tipo, creada from notas where id in (select value from json_each(?1))",
    )
    .bind(base::a_json(&ids))
    .fetch_all(&mut *conn)
    .await?;
    Ok(cercanas
        .into_iter()
        .filter_map(|(dato, parecido)| {
            let (id, titulo, cuerpo, tipo, creada) = filas.iter().find(|f| f.0 == dato.id)?.clone();
            Some(Candidata { id, titulo, cuerpo, tipo, creada, parecido })
        })
        .collect())
}

/// Deja al día las palabras por las que se encuentra la nota.
async fn indexar(conn: &mut SqliteConnection, id: i64, titulo: &str, cuerpo: &str) -> Resultado<()> {
    sqlx::query("delete from notas_texto where rowid = ?1").bind(id).execute(&mut *conn).await?;
    sqlx::query("insert into notas_texto (rowid, titulo, cuerpo) values (?1, ?2, ?3)")
        .bind(id)
        .bind(texto::para_indexar(titulo))
        .bind(texto::para_indexar(cuerpo))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Vuelve a armar las palabras de todas las notas (cuando faltan, o cuando
/// cambió la forma de sacarles la raíz).
pub async fn reindexar(conn: &mut SqliteConnection) -> Resultado<u64> {
    sqlx::query("delete from notas_texto").execute(&mut *conn).await?;
    let notas: Vec<(i64, String, String)> =
        sqlx::query_as("select id, titulo, cuerpo from notas order by id").fetch_all(&mut *conn).await?;
    for (id, titulo, cuerpo) in &notas {
        sqlx::query("insert into notas_texto (rowid, titulo, cuerpo) values (?1, ?2, ?3)")
            .bind(id)
            .bind(texto::para_indexar(titulo))
            .bind(texto::para_indexar(cuerpo))
            .execute(&mut *conn)
            .await?;
    }
    Ok(notas.len() as u64)
}

/// Suma a una lista guardada los valores nuevos, sin repetir.
fn sumar(guardada: &str, nuevos: &[String]) -> String {
    base::a_json(&base::unir(&base::lista(guardada), nuevos))
}

/// Si ya hay una vigente igual (mismo tipo, título y cuerpo), le suma el
/// origen y devuelve su id.
async fn repetida_exacta(
    conn: &mut SqliteConnection,
    grupo: i64,
    nota: &NotaLista,
    ahora: DateTime<Utc>,
) -> Resultado<Option<i64>> {
    let iguales: Vec<(i64, String, String)> = sqlx::query_as(
        "select id, titulo, origenes from notas
         where grupo_id = ?1 and estado = 'vigente' and tipo = ?2 and cuerpo = ?3
         order by id",
    )
    .bind(grupo)
    .bind(nota.tipo.clave())
    .bind(&nota.cuerpo)
    .fetch_all(&mut *conn)
    .await?;
    // El título se compara acá, que entiende de mayúsculas con acento.
    let titulo = nota.titulo.to_lowercase();
    let Some((id, _, origenes)) = iguales.into_iter().find(|(_, t, _)| t.to_lowercase() == titulo) else {
        return Ok(None);
    };
    sqlx::query("update notas set origenes = ?2, actualizada = ?3 where id = ?1")
        .bind(id)
        .bind(sumar(&origenes, &nota.origenes))
        .bind(ahora)
        .execute(&mut *conn)
        .await?;
    Ok(Some(id))
}

/// Deja una nota vigente con el texto de la nueva, que dice lo mismo, y le
/// suma su origen, sus repositorios y sus etiquetas.
async fn unir(
    conn: &mut SqliteConnection,
    id: i64,
    nota: &NotaLista,
    huella: &[f32],
    ahora: DateTime<Utc>,
) -> Resultado<()> {
    let (origenes, repos, etiquetas): (String, String, String) =
        sqlx::query_as("select origenes, repos, etiquetas from notas where id = ?1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    sqlx::query(
        "update notas set titulo = ?2, cuerpo = ?3, huella = ?4, origenes = ?5, repos = ?6, etiquetas = ?7,
                          actualizada = ?8
         where id = ?1",
    )
    .bind(id)
    .bind(&nota.titulo)
    .bind(&nota.cuerpo)
    .bind(indice::a_bytes(huella))
    .bind(sumar(&origenes, &nota.origenes))
    .bind(sumar(&repos, &nota.repos))
    .bind(sumar(&etiquetas, &nota.etiquetas))
    .bind(ahora)
    .execute(&mut *conn)
    .await?;
    indexar(conn, id, &nota.titulo, &nota.cuerpo).await
}

/// Guarda la nota. `huella` es la de su texto; sin ella (las huellas no
/// andaban) solo se reconocen las repetidas exactas y la huella se completa
/// después.
pub async fn guardar(e: &Estado, grupo: &Grupo, nota: &NotaLista, huella: Option<&[f32]>) -> Resultado<Guardado> {
    let tipo = nota.tipo.clave();
    let ahora = base::ahora();
    let mut tx = base::escribir(&e.pool).await?;
    if let Some(id) = repetida_exacta(&mut tx, grupo.id, nota, ahora).await? {
        tx.commit().await?;
        return Ok(Guardado::Repetida { id });
    }

    if let Some(vieja) = nota.reemplaza {
        let estado: Option<String> =
            sqlx::query_scalar("select estado from notas where id = ?1").bind(vieja).fetch_optional(&mut *tx).await?;
        match estado.as_deref() {
            None => return uso(format!("No existe la nota {vieja}, así que no puede reemplazarse.")),
            Some("vigente") => {}
            Some(otro) => return uso(format!("La nota {vieja} ya está {otro}: no se puede reemplazar.")),
        }
    }

    let mut parecida = None;
    if let Some(huella) = huella {
        let cercanas = candidatas(&mut tx, &e.indice, grupo.id, huella, nota.reemplaza).await?;
        let decision = if nota.reemplaza.is_some() {
            // Con `reemplaza`, Claude ya dijo qué hacer con la anterior: solo se avisa.
            match cercanas.first() {
                Some(c) if c.parecido >= e.umbrales.aviso => Decision::Avisar(0),
                _ => Decision::Nada,
            }
        } else {
            decidir(&cercanas, tipo, &nota.texto(), &e.umbrales)
        };
        match decision {
            Decision::Unir(i) => {
                let c = &cercanas[i];
                unir(&mut tx, c.id, nota, huella, ahora).await?;
                tx.commit().await?;
                e.indice.poner(
                    Clase::Notas,
                    Dato { id: c.id, grupo: Some(grupo.id), fecha: c.creada.timestamp() },
                    huella,
                );
                tracing::info!(nota = c.id, parecido = c.parecido, anterior = %c.titulo, "nota unida a una casi igual");
                return Ok(Guardado::Unida { id: c.id, titulo_anterior: c.titulo.clone() });
            }
            Decision::Avisar(i) => {
                let c = &cercanas[i];
                parecida = Some(Parecida { id: c.id, titulo: c.titulo.clone(), parecido: c.parecido });
            }
            Decision::Nada => {}
        }
    }

    let id: i64 = sqlx::query_scalar(
        "insert into notas (grupo_id, tipo, titulo, cuerpo, origenes, autor, repos, etiquetas, creada, actualizada, huella)
         values (?1, ?2, ?3, ?4, ?5, 'claude', ?6, ?7, ?8, ?8, ?9)
         returning id",
    )
    .bind(grupo.id)
    .bind(tipo)
    .bind(&nota.titulo)
    .bind(&nota.cuerpo)
    .bind(base::a_json(&nota.origenes))
    .bind(base::a_json(&nota.repos))
    .bind(base::a_json(&nota.etiquetas))
    .bind(ahora)
    .bind(huella.map(indice::a_bytes))
    .fetch_one(&mut *tx)
    .await?;
    indexar(&mut tx, id, &nota.titulo, &nota.cuerpo).await?;
    if let Some(vieja) = nota.reemplaza {
        sqlx::query("update notas set estado = 'reemplazada', reemplazada_por = ?2, actualizada = ?3 where id = ?1")
            .bind(vieja)
            .bind(id)
            .bind(ahora)
            .execute(&mut *tx)
            .await?;
    }
    // La huella queda lista para comparar antes de soltar el turno de
    // escritura: la próxima nota que entre ya la ve.
    if let Some(huella) = huella {
        e.indice.poner(Clase::Notas, Dato { id, grupo: Some(grupo.id), fecha: ahora.timestamp() }, huella);
    }
    if let Err(error) = tx.commit().await {
        e.indice.quitar(Clase::Notas, id);
        return Err(error.into());
    }
    Ok(Guardado::Nueva { id, reemplazo: nota.reemplaza, parecida })
}

/// Los títulos de las dos notas de un reemplazo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reemplazo {
    pub vieja: String,
    pub nueva: String,
}

/// Deja una nota vigente como reemplazada por otra, que pasa a ser la que vale.
pub async fn reemplazar(pool: &SqlitePool, vieja: i64, nueva: i64) -> Resultado<Reemplazo> {
    if vieja == nueva {
        return uso("Una nota no puede reemplazarse a sí misma.");
    }
    let mut tx = base::escribir(pool).await?;
    let filas: Vec<(i64, String, String)> =
        sqlx::query_as("select id, titulo, estado from notas where id in (?1, ?2) order by id")
            .bind(vieja)
            .bind(nueva)
            .fetch_all(&mut *tx)
            .await?;
    let buscar = |id: i64| filas.iter().find(|f| f.0 == id);
    let Some((_, titulo_vieja, estado_vieja)) = buscar(vieja) else {
        return uso(format!("No existe la nota {vieja}."));
    };
    let Some((_, titulo_nueva, estado_nueva)) = buscar(nueva) else {
        return uso(format!("No existe la nota {nueva}."));
    };
    if estado_vieja != "vigente" {
        return uso(format!("La nota {vieja} ya está {estado_vieja}: no se puede reemplazar."));
    }
    if estado_nueva != "vigente" {
        return uso(format!("La nota {nueva} está {estado_nueva}: no puede reemplazar a otra."));
    }
    sqlx::query("update notas set estado = 'reemplazada', reemplazada_por = ?2, actualizada = ?3 where id = ?1")
        .bind(vieja)
        .bind(nueva)
        .bind(base::ahora())
        .execute(&mut *tx)
        .await?;
    let reemplazo = Reemplazo { vieja: titulo_vieja.clone(), nueva: titulo_nueva.clone() };
    tx.commit().await?;
    Ok(reemplazo)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn reconoce_claves_y_tokens() {
        assert_eq!(buscar_secreto("la clave es AKIAABCDEFGHIJKLMNOP"), Some("una clave de acceso de AWS"));
        assert_eq!(buscar_secreto("-----BEGIN OPENSSH PRIVATE KEY-----"), Some("una clave privada"));
        assert_eq!(buscar_secreto("ghp_abcdefghijklmnopqrstuvwxyz0123456789"), Some("un token de GitHub"));
        assert_eq!(
            buscar_secreto("postgres://ana:secreto123@db.local/app"),
            Some("una contraseña dentro de una dirección")
        );
        assert_eq!(buscar_secreto("password = Hx7k29sLmQ0pz"), Some("una contraseña o un token"));
    }

    #[test]
    fn no_confunde_explicaciones_con_secretos() {
        assert_eq!(buscar_secreto("La contraseña: guardada en Secrets Manager"), None);
        assert_eq!(buscar_secreto("clave: usar Cognito para la sesión"), None);
        assert_eq!(buscar_secreto("postgres://ana@localhost:5432/memoria"), None);
        assert_eq!(buscar_secreto("token: lo renueva el job nocturno"), None);
    }

    #[test]
    fn tapa_los_secretos_y_deja_el_resto() {
        let limpio = "Usuario: el deploy falla en us-east-2";
        assert!(matches!(tapar_secretos(limpio), Cow::Borrowed(_)));
        let tapado =
            tapar_secretos("probá con ghp_abcdefghijklmnopqrstuvwxyz0123456789 y password = Hx7k29sLmQ0pz, dale");
        assert_eq!(tapado, format!("probá con {TAPADO} y password = {TAPADO} dale"));
        assert_eq!(buscar_secreto(&tapado), None);
        let direccion = tapar_secretos("la base es postgres://ana:secreto123@db.local/app");
        assert_eq!(direccion, format!("la base es {TAPADO}db.local/app"));
    }

    #[test]
    fn limpia_titulos_y_listas() {
        assert_eq!(una_linea("  Usar\nCognito   para  la sesión "), "Usar Cognito para la sesión");
        let etiquetas = lista(&["Auth".into(), " auth ".into(), String::new(), "Login".into()], true);
        assert_eq!(etiquetas, vec!["auth", "login"]);
    }

    fn duros(texto: &str) -> Vec<String> {
        datos_duros(texto).into_iter().collect()
    }

    #[test]
    fn saca_los_datos_duros() {
        assert_eq!(
            duros("Deploy falla por la región\nError: el deploy usa us-east-2. Solución: fijar AWS_REGION."),
            vec!["aws_region", "us-east-2"]
        );
        assert_eq!(
            duros("Se facturaron 62 horas a Globex en octubre. La factura salió el 3/11."),
            vec!["3/11", "62", "globex", "octubre"]
        );
        assert_eq!(duros("Entregas los Viernes y los miércoles"), vec!["miercoles", "viernes"]);
        assert_eq!(
            duros("PORTAL-751 Pipeline ALFA en producción (12/11)"),
            vec!["12/11", "alfa", "pipeline", "portal-751"]
        );
        // La primera palabra de cada oración lleva mayúscula sin ser un nombre.
        assert_eq!(
            duros("Qué: la sesión pasa a otro proveedor. Por qué: costo. Cómo: con el pool."),
            Vec::<String>::new()
        );
    }

    #[test]
    fn chocan_solo_si_cada_una_tiene_algo_que_la_otra_no() {
        let octubre = datos_duros("Se facturaron 62 horas en octubre");
        let noviembre = datos_duros("Se facturaron 48 horas en noviembre");
        assert!(chocan(&octubre, &noviembre));
        let corta = datos_duros("Se entrega cada viernes");
        let larga = datos_duros("Se entrega cada viernes antes de las 12");
        assert!(!chocan(&corta, &larga), "sumar un dato no es contradecir");
        assert!(!chocan(&corta, &datos_duros("Las entregas son semanales")), "sin datos de un lado no hay choque");
    }

    fn candidata(id: i64, tipo: &str, texto: &str, parecido: f64) -> Candidata {
        Candidata {
            id,
            titulo: texto.into(),
            cuerpo: "Cuerpo.".into(),
            tipo: tipo.into(),
            creada: Utc::now(),
            parecido,
        }
    }

    #[test]
    fn decide_unir_avisar_o_nada() {
        let u = Umbrales::default();
        let nueva = "Se entrega cada viernes antes de las 12";
        let une = [candidata(1, "acuerdo", "Entregas los viernes", 0.94)];
        assert!(matches!(decidir(&une, "acuerdo", nueva, &u), Decision::Unir(0)));
        // Otro tipo: no se pisa, se avisa.
        assert!(matches!(decidir(&une, "dato", nueva, &u), Decision::Avisar(0)));
        // Casi iguales, pero cambia el día: son dos hechos.
        let otro_dia = [candidata(1, "acuerdo", "Entregas los lunes", 0.93)];
        assert!(matches!(decidir(&otro_dia, "acuerdo", nueva, &u), Decision::Avisar(0)));
        // La más cercana choca y la segunda no: se une a la segunda.
        let dos =
            [candidata(1, "acuerdo", "Entregas los lunes", 0.95), candidata(2, "acuerdo", "Entregas semanales", 0.91)];
        assert!(matches!(decidir(&dos, "acuerdo", nueva, &u), Decision::Unir(1)));
        let parecida = [candidata(1, "acuerdo", "Entregas", 0.85)];
        assert!(matches!(decidir(&parecida, "acuerdo", nueva, &u), Decision::Avisar(0)));
        let lejana = [candidata(1, "acuerdo", "Otra cosa", 0.62)];
        assert!(matches!(decidir(&lejana, "acuerdo", nueva, &u), Decision::Nada));
        assert!(matches!(decidir(&[], "acuerdo", nueva, &u), Decision::Nada));
    }
}
