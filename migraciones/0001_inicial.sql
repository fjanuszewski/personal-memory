-- Esquema de la memoria en SQLite: todo vive en un solo archivo.
--
-- Cómo se guarda cada cosa:
--   - Las listas (carpetas, orígenes, etiquetas…) van como texto JSON.
--   - Las fechas van como texto, en UTC («2026-11-13T18:27:01.745+00:00»):
--     las escribe siempre el programa, así se pueden comparar como texto.
--   - `proyecto` vacío quiere decir «la empresa en sí».
--   - Las huellas van como números de 4 bytes, uno tras otro. Para buscar se
--     cargan en memoria al arrancar.
--   - Para buscar por palabras hay dos tablas aparte (`notas_texto` y
--     `fragmentos_texto`) con las raíces de cada texto; las llena el programa.

create table grupos (
  id          integer primary key,
  empresa     text not null,                 -- 'ACME'
  proyecto    text not null default '',      -- 'Portal'; '' = la empresa en sí
  carpetas    text not null default '[]',    -- carpetas que caen acá
  grupos_app  text not null default '[]',    -- nombres del costado: '[ACME] - PORTAL'
  unique (empresa, proyecto)
);

-- Conversaciones conocidas, su grupo y hasta dónde se leyeron.
create table conversaciones (
  id             text primary key,             -- id de la app (local_…)
  transcripcion  text,                         -- id del archivo .jsonl
  titulo         text,
  carpeta        text,
  grupo_id       integer references grupos,
  leida_hasta    integer not null default 0,   -- byte del archivo ya procesado
  actualizada    text not null
);

create table notas (
  id               integer primary key autoincrement,
  grupo_id         integer not null references grupos,
  tipo             text not null check (tipo in ('decision', 'error', 'cambio', 'acuerdo', 'dato', 'pendiente')),
  titulo           text not null,              -- una línea
  cuerpo           text not null,              -- qué, por qué, cómo
  estado           text not null default 'vigente' check (estado in ('vigente', 'reemplazada', 'descartada')),
  reemplazada_por  integer references notas,
  origenes         text not null default '[]', -- conversaciones de donde salió
  autor            text not null,              -- 'claude' | 'local'
  repos            text not null default '[]',
  etiquetas        text not null default '[]',
  creada           text not null,
  actualizada      text not null,
  huella           blob                        -- huella de significado
);
create index notas_del_grupo on notas (grupo_id, estado, creada desc);
create index notas_sin_huella on notas (id) where huella is null;
create virtual table notas_texto using fts5 (titulo, cuerpo, tokenize = "unicode61 remove_diacritics 2");

-- Segunda capa: pedazos de conversaciones, para lo que nunca llegó a nota.
create table fragmentos (
  id            integer primary key autoincrement,
  conversacion  text not null references conversaciones,
  grupo_id      integer references grupos,
  desde         integer not null,              -- posición en el archivo
  hasta         integer not null,
  contenido     text not null,
  fecha         text not null,
  marca         text not null,                 -- huella corta del texto, para no guardar dos veces lo mismo
  huella        blob
);
create index fragmentos_de_la_conversacion on fragmentos (conversacion);
create index fragmentos_del_grupo on fragmentos (grupo_id, fecha desc);
create index fragmentos_por_marca on fragmentos (marca);
create index fragmentos_sin_huella on fragmentos (id) where huella is null;
create virtual table fragmentos_texto using fts5 (contenido, tokenize = "unicode61 remove_diacritics 2");
-- En cuántos pedazos aparece cada palabra: sirve para empezar a buscar por
-- las menos comunes. Se calcula sola a partir de la tabla de arriba.
create virtual table fragmentos_palabras using fts5vocab (fragmentos_texto, row);

-- Mejoras: cada pedido, su estado y lo que se hizo.
create table mejoras (
  id               integer primary key autoincrement,
  resumen          text not null,   -- 1 o 2 oraciones: lo que se le pregunta al usuario
  detalle          text not null,   -- qué falló, con ejemplo: buscado / esperado / obtenido
  firma            text not null,   -- reconoce el mismo problema cuando reaparece
  origen           text not null,   -- 'claude' | 'servicio'
  conversacion     text references conversaciones, -- dónde se preguntó y dónde se avisa
  grupo_id         integer references grupos,      -- el grupo donde se notó la falla
  ejemplo          text not null default '{}',     -- buscado, esperado, obtenido y notas esperadas
  toca_base        integer not null default 0,
  borra_datos      integer not null default 0,
  suma_librerias   integer not null default 0,
  estado           text not null default 'propuesta'
                     check (estado in ('propuesta', 'aceptada', 'en_curso', 'instalada', 'fallida', 'rechazada')),
  motivo           text,            -- por qué falló o se rechazó
  cambios          text,            -- resumen de lo que cambió el agente
  version_antes    text,
  version_despues  text,
  puntaje_antes    real,
  puntaje_despues  real,
  intentos         integer not null default 0,     -- cuántas veces se lanzó el agente
  rama             text,                           -- la rama de git donde quedó el cambio
  avisada          integer not null default 0,
  creada           text not null,
  empezada         text,                           -- cuándo la tomó el guardián
  terminada        text
);
create index mejoras_por_estado on mejoras (estado, creada);
create index mejoras_por_firma on mejoras (firma);

-- Señales para que el servicio detecte fallas sin depender de Claude.
create table senales (
  id            integer primary key autoincrement,
  tipo          text not null,      -- 'busqueda' | 'busqueda_vacia' | 'lenta' | 'error'
  conversacion  text references conversaciones,
  detalle       text not null,      -- JSON
  fecha         text not null
);
create index senales_por_tipo on senales (tipo, fecha desc);

-- Batería: preguntas con respuesta esperada. La escribe el guardián, nunca el agente.
create table bateria (
  id            integer primary key autoincrement,
  pregunta      text not null,
  grupo_id      integer references grupos,
  esperadas     text not null,      -- JSON: notas que tienen que aparecer
  desde_mejora  integer references mejoras,
  creada        text not null
);
-- Una misma pregunta no entra dos veces.
create unique index bateria_sin_repetir on bateria (pregunta, coalesce(grupo_id, 0));

-- Valores sueltos que el servicio recuerda entre arranques (por ejemplo, con
-- qué modelo se calcularon las huellas: si cambia, hay que recalcularlas).
create table ajustes (
  clave  text primary key,
  valor  text not null
);
