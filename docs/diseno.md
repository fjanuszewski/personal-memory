# Memoria por grupo — Documento de diseño

Estado: borrador para revisar · 12/11/2026

## 1. Qué es

Un servicio que corre siempre de fondo en esta Mac y guarda, por grupo (empresa › proyecto, por ejemplo ACME › Portal), lo que conviene recordar del trabajo con Claude: decisiones, errores con su solución, cambios, acuerdos con el cliente y lo conversado. Claude lo consulta y lo completa en cada conversación sin que el usuario tenga que hacer nada.

Además se mejora solo. Cuando Claude nota que la memoria falla, lo propone en el mismo chat en una o dos oraciones. Si el usuario acepta, un agente lo arregla de fondo y el resultado vuelve a ese chat.

**Qué no es:**
- No tiene pantallas propias. Si hace falta mirar adentro, se abre la base con DBeaver.
- No reemplaza la documentación técnica de cada repositorio. Las decisiones de arquitectura de un sistema siguen viviendo en su repo (`docs/adr`); la memoria guarda lo demás y lo que cruza proyectos.
- No sale de esta Mac, por ahora.

## 2. Decisiones tomadas

| Tema | Decisión |
|---|---|
| Dónde corre | Solo en esta Mac, de fondo, arranca solo con la sesión de macOS |
| Pantallas | Ninguna |
| Base de datos | Un archivo de SQLite (`~/.memoria/memoria.db`) con respaldo diario. Hasta la versión 0.3 fue Postgres 18 con pgvector; se cambió para no depender de un motor instalado (4.12) |
| Huellas de significado | Un modelo que corre dentro del servicio (EmbeddingGemma 300M comprimido, por defecto). Hasta la 0.3 las calculaba Ollama con `bge-m3`, que queda como opción (4.12) |
| Programa | Servicio en Rust |
| Conexión con Claude | Servidor MCP (la forma estándar en que Claude usa herramientas externas), instalado para todas las conversaciones |
| Grupos | Empresa › proyecto. Salen del nombre del grupo en el costado de la app o, si no tiene, de la carpeta; no hay nombres escritos en el programa (4.12) |
| Quién escribe las notas | Claude durante la conversación; un modelo local de noche para lo viejo y lo que se escape |
| Calidad | Sin revisión manual: se unen las repetidas, se marcan las reemplazadas y se guarda el origen |
| Mejoras | Pregunta corta en el chat → agente inmediato de fondo → línea con el resultado en el mismo chat → Claude retoma lo que había fallado |

## 3. Vista general

```
 Chat de Claude (ACME › Portal)                    Servicio "memoria" (Rust, de fondo)
 ┌───────────────────────────┐  MCP (HTTP local)  ┌─────────────────────────────┐
 │ Claude                    │ ─────────────────▶ │ Herramientas para Claude     │
 │  · busca y guarda notas   │ ◀───────────────── │ Búsqueda híbrida             │
 │  · reporta fallas         │                    │ Calidad (repetidas, etc.)    │
 └───────────▲───────────────┘                    │ Lector de conversaciones     │
             │ resumen al empezar,                │ Señales de falla             │
             │ línea de resultado                 └──────┬───────────────▲──────┘
 ┌───────────┴───────────────┐                           │               │ pide mejora
 │ Mod "memoria"             │                    ┌──────▼──────┐  ┌─────┴──────────────┐
 │ (puente con la app)       │                    │ Postgres +  │  │ Guardián (Rust)     │
 │  · grupo de la sesión     │                    │ pgvector    │  │  · agente de        │
 │  · resumen al empezar     │                    └─────────────┘  │    mantenimiento    │
 │  · aviso + reintento      │                    ┌─────────────┐  │  · batería, prueba, │
 └───────────────────────────┘                    │ Ollama      │  │    instalación y    │
                                                  │ (huellas y  │  │    vuelta atrás     │
 Conversaciones guardadas en ~/.claude/projects   │ modelo      │  └─────────────────────┘
 (las lee el servicio)                            │ local)      │
                                                  └─────────────┘
```

Son dos programas en Rust:
- **El servicio** es lo que usa Claude y lo que el agente puede mejorar.
- **El guardián** lanza el agente, prueba lo que hizo, lo instala y vuelve atrás si algo falla. El agente no lo puede tocar.

## 4. Piezas

### 4.1 Base de datos

> **Desde la versión 0.4 la base es un archivo de SQLite**, no Postgres. Lo que sigue en este punto es el diseño original y sirve para entender qué guarda cada tabla; cómo quedó hoy está en 4.12, y el esquema vigente, en `migraciones/0001_inicial.sql`.

Base `memoria` en Postgres.app 18. Extensiones: `vector` (pgvector 0.8.1, con índice HNSW para búsqueda rápida por significado) y `unaccent` (búsqueda sin importar los acentos).

`unaccent` no se puede usar directo en una columna calculada, así que se arma una configuración de texto propia que ya quita los acentos:

```sql
create text search configuration es (copy = spanish);
alter text search configuration es
  alter mapping for hword, hword_part, word with unaccent, spanish_stem;
```

Esquema inicial:

```sql
-- Grupos: empresa y proyecto. proyecto null = la empresa en sí.
create table grupos (
  id          bigserial primary key,
  empresa     text not null,                 -- 'ACME'
  proyecto    text,                          -- 'Portal'
  carpetas    text[] not null default '{}',  -- carpetas que caen acá
  grupos_app  text[] not null default '{}',  -- nombres del costado: '[ACME] - PORTAL'
  unique nulls not distinct (empresa, proyecto)  -- sin esto, «ACME» (proyecto null) podría repetirse
);

-- Conversaciones conocidas, su grupo y hasta dónde se leyeron.
create table conversaciones (
  id             text primary key,             -- id de la app (local_…)
  transcripcion  text,                         -- id del archivo .jsonl
  titulo         text,
  carpeta        text,
  grupo_id       bigint references grupos,
  leida_hasta    bigint not null default 0,    -- byte del archivo ya procesado
  actualizada    timestamptz not null default now()
);

create type tipo_nota   as enum ('decision', 'error', 'cambio', 'acuerdo', 'dato', 'pendiente');
create type estado_nota as enum ('vigente', 'reemplazada', 'descartada');

create table notas (
  id               bigserial primary key,
  grupo_id         bigint not null references grupos,
  tipo             tipo_nota not null,
  titulo           text not null,              -- una línea
  cuerpo           text not null,              -- qué, por qué, cómo
  estado           estado_nota not null default 'vigente',
  reemplazada_por  bigint references notas,
  origenes         text[] not null default '{}', -- conversaciones de donde salió
  autor            text not null,              -- 'claude' | 'local'
  repos            text[] not null default '{}',
  etiquetas        text[] not null default '{}',
  creada           timestamptz not null default now(),
  actualizada      timestamptz not null default now(),
  texto            tsvector generated always as (
                     setweight(to_tsvector('es', titulo), 'A') ||
                     setweight(to_tsvector('es', cuerpo), 'B')) stored,
  huella           vector(1024)                -- huella de significado
);
create index on notas using gin (texto);
create index on notas using hnsw (huella vector_cosine_ops);
create index on notas (grupo_id, estado, creada desc);

-- Segunda capa: pedazos de conversaciones, para lo que nunca llegó a nota.
create table fragmentos (
  id            bigserial primary key,
  conversacion  text not null references conversaciones,
  grupo_id      bigint references grupos,
  desde         bigint not null,               -- posición en el archivo
  hasta         bigint not null,
  contenido     text not null,
  fecha         timestamptz not null,
  texto         tsvector generated always as (to_tsvector('es', contenido)) stored,
  huella        vector(1024)
);
create index on fragmentos using gin (texto);
create index on fragmentos using hnsw (huella vector_cosine_ops);

-- Mejoras: cada pedido, su estado y lo que se hizo.
create type estado_mejora as enum
  ('propuesta', 'aceptada', 'en_curso', 'instalada', 'fallida', 'rechazada');
create table mejoras (
  id               bigserial primary key,
  resumen          text not null,   -- 1 o 2 oraciones: lo que se le pregunta al usuario
  detalle          text not null,   -- qué falló, con ejemplo: buscado / esperado / obtenido
  firma            text not null,   -- reconoce el mismo problema cuando reaparece
  origen           text not null,   -- 'claude' | 'servicio'
  conversacion     text references conversaciones, -- dónde se preguntó y dónde se avisa
  toca_base        boolean not null default false,
  borra_datos      boolean not null default false,
  suma_librerias   boolean not null default false,
  estado           estado_mejora not null default 'propuesta',
  motivo           text,            -- por qué falló o se rechazó
  cambios          text,            -- resumen de lo que cambió el agente
  version_antes    text,
  version_despues  text,
  puntaje_antes    real,
  puntaje_despues  real,
  avisada          boolean not null default false,
  creada           timestamptz not null default now(),
  terminada        timestamptz
);

-- Señales para que el servicio detecte fallas sin depender de Claude.
create table senales (
  id            bigserial primary key,
  tipo          text not null,      -- 'busqueda_vacia' | 'resultado_no_usado' | 'repetida' | 'lenta' | 'error'
  conversacion  text references conversaciones,
  detalle       jsonb not null,
  fecha         timestamptz not null default now()
);

-- Batería: preguntas con respuesta esperada. La escribe el guardián, nunca el agente.
create table bateria (
  id            bigserial primary key,
  pregunta      text not null,
  grupo_id      bigint references grupos,
  esperadas     bigint[] not null,  -- notas que tienen que aparecer
  desde_mejora  bigint references mejoras,
  creada        timestamptz not null default now()
);
```

Los cambios de estructura se hacen con migraciones numeradas en `migraciones/`. Antes de aplicar cualquiera, se hace un respaldo.

### 4.2 El servicio `memoria` (Rust)

- **Proceso y conexión:** un solo programa, que arranca con la sesión de macOS (`~/Library/LaunchAgents/personal-memory.memoria.plist`, siempre vivo). Escucha solo en `127.0.0.1:7337` y pide una clave local.
- **Librerías:**
  - `tokio`: tareas en paralelo.
  - `rmcp`: kit oficial de MCP para Rust, por HTTP.
  - `sqlx`: la base. Hasta la 0.3, Postgres con las consultas revisadas al compilar; desde la 0.4, SQLite con consultas comunes, cubiertas por pruebas.
  - `fastembed`: corre el modelo de huellas dentro del programa (desde la 0.4).
  - `rust-stemmers`: reduce las palabras a su raíz para buscar (desde la 0.4; antes lo hacía Postgres).
  - `notify`: vigila archivos. *(No hizo falta: el lector revisa cada minuto el tamaño de los archivos de las conversaciones registradas, que es más simple y no pierde cambios.)*
  - `reqwest`: habla con Ollama, si se elige usarlo.
  - `serde`: lee y escribe datos.
  - `tracing`: registros.
- **Partes:**
  - `mcp`: herramientas para Claude (4.3).
  - `buscar`: búsqueda híbrida (4.6).
  - `notas`: altas, unión de repetidas, reemplazos y filtro de secretos (4.5).
  - `lector`: lee las conversaciones nuevas y arma fragmentos (4.7).
  - `huellas`: genera las huellas de significado. Va detrás de una interfaz: hasta la 0.3 solo con Ollama (`bge-m3`, multilingüe, 1024 dimensiones); desde la 0.4, por defecto con un modelo dentro del programa (`interno`), y Ollama como opción.
  - `texto`, `indice` y `base` (desde la 0.4): las raíces de las palabras, las huellas en memoria y el manejo del archivo de la base.
  - `nocturno`: extracción de notas con el modelo local (4.7).
  - `senales`: detección de fallas (4.8).
  - `respaldo`: una copia entera de la base por día; se guardan 14 días en `~/.memoria/respaldos/`.
- **Registros:** `~/Library/Logs/memoria/`.

### 4.3 Herramientas para Claude (MCP)

Se instala una vez para todas las conversaciones:

```bash
claude mcp add --transport http --scope user memoria http://127.0.0.1:7337/mcp -H "Authorization: Bearer <clave local>"
```

| Herramienta | Para qué |
|---|---|
| `memoria_contexto(sesion)` | Grupo de la conversación y resumen corto: decisiones vigentes recientes, errores abiertos, pendientes. Máximo unas 30 líneas. |
| `memoria_buscar(consulta, grupo?, tipo?, desde?, conversaciones=true, reemplazadas=false, limite=8)` | Resultados con id, tipo, título, extracto, fecha, estado y origen. |
| `memoria_guardar(tipo, titulo, cuerpo, grupo?, repos?, etiquetas?, reemplaza?)` | Crea la nota. Si es casi igual a otra la une; si se parece, avisa cuál. |
| `memoria_reemplazar(vieja, nueva)` | Marca una decisión como reemplazada por otra. |
| `memoria_reportar(resumen, detalle, buscado?, esperado?, obtenido?)` | Registra una falla. Responde si hay que preguntarle al usuario o si ya está en curso o rechazada. |
| `memoria_mejorar(id)` / `memoria_rechazar(id)` | Lo que el usuario contestó a la pregunta de mejora. |
| `memoria_mejora_estado(id)` | En qué está una mejora. |

Por defecto todo usa el grupo de la conversación: el servicio lo conoce por el id de sesión que le pasa el mod.

### 4.4 Conexión con la app: el mod `memoria`

Es un mod propio, que se instala desde el catálogo local de este repositorio (`memoria@personal-memory`).

- **Al empezar o al conectarse la app:**
  1. Lee los datos de su propia conversación: grupo del costado, carpeta e id.
  2. Los registra en el servicio y recibe el resumen del grupo.
  3. Lo agrega como sección de las instrucciones de esa conversación, junto con las reglas de uso (abajo) y el id de sesión.
- **Cambios a mitad de la conversación:** la app arma las instrucciones una sola vez por conversación (y de nuevo recién al resumirla). El grupo se revisa antes de cada mensaje. Si la conversación pasó a otro grupo del costado, el resumen llegó tarde o el mod se cargó con la conversación ya empezada, el mod se lo adjunta a Claude al próximo mensaje, venga de donde venga (del usuario, de otra conversación o un aviso de la app), una sola vez.
- **El agente de cada mejora corre acá** (desde el 14/11/2026, ver 4.8): cuando el guardián deja lista la copia del código de una mejora aceptada en esta conversación, el mod lanza al agente de fondo (`$.agent.spawn`), le pone los límites (`tool.call`), decide él cada permiso sin que la app le pregunte al usuario (`tool.check`) y le anota al servicio cuándo empezó, que sigue andando y cómo terminó.
- **Mejoras en curso:** mientras haya alguna lanzada desde esta conversación, el mod pregunta su estado cada 10 segundos. Cuando termina:
  - Pone una línea visible en el chat (`$.session.append`, como aviso): *"Memoria: mejora instalada, ahora encuentra sinónimos"* o *"No pude…; quedó como estaba"*.
  - Si se instaló, manda un mensaje corto para que Claude retome lo que había fallado (`$.prompt.submit`). Si Claude está en medio de un turno, el mensaje queda en fila.
- **Si la conversación se cerró antes:**
  - El aviso queda pendiente (`avisada = false`) y aparece al volver a abrirla.
  - Si nunca se reabre, aparece en la próxima conversación del mismo grupo.
- **Conversaciones fuera de la app (terminal):** las herramientas funcionan igual y el grupo sale por la carpeta. El resumen automático necesita un gancho de inicio en la configuración (etapa 5).

**Reglas de uso que recibe Claude en cada conversación (borrador):**
- Antes de responder sobre algo que pudo decidirse antes en este grupo, buscá en la memoria.
- Cuando se decide algo, se resuelve un error, se acuerda algo con el cliente o se hace un cambio importante, guardá una nota corta: qué, por qué y cómo. Si reemplaza una anterior, indicalo.
- Si la memoria no te dio lo que necesitabas, o te dio algo viejo o incorrecto, reportalo. Si la respuesta dice que hay que preguntar, preguntale al usuario en una o dos oraciones si quiere la mejora, aclarando si toca la base, borra datos o suma librerías. Si acepta, llamá `memoria_mejorar`.
- Nunca guardes claves, contraseñas, tokens ni datos personales de terceros.

### 4.5 Notas: formato y calidad

- **Tipos:**
  - decisión
  - error (con causa y solución)
  - cambio
  - acuerdo (con el cliente)
  - dato (un hecho útil)
  - pendiente
- **Cuerpo:** *Qué / Por qué / Cómo*, o *Error / Causa / Solución*. Corto: si hace falta más, se enlaza al repo o a la conversación de origen.
- **Repetidas:** al guardar, se buscan notas vigentes del mismo grupo.
  - Con parecido de 0,90 o más (por significado), se unen: se actualiza el cuerpo y se suma el origen.
  - Entre 0,80 y 0,90, se crea igual pero la respuesta dice "parecida a la nota 123", para que Claude decida si reemplaza.
  - Los umbrales son ajustables y son de lo primero que puede afinar el agente.
  - Solo se unen notas del mismo tipo, y nunca dos que cambian un dato duro (un número, una fecha, un día, un mes, un identificador o un nombre propio): «la factura de octubre» y «la de noviembre» se parecen 0,94 y son dos hechos. En esos casos se guarda aparte y se avisa.
- **Reemplazos:** una decisión nueva sobre el mismo tema deja la vieja como `reemplazada`, enlazada a la nueva. La búsqueda muestra primero las vigentes; las reemplazadas solo aparecen si se piden o como "antes se usaba X". Si lo que coincide con la búsqueda es la reemplazada, se muestra la que vale hoy (siguiendo la cadena de reemplazos) con la vieja debajo.
- **Origen:** toda nota guarda de qué conversaciones salió.
- **Secretos:** antes de guardar se buscan claves y tokens con patrones conocidos. Si aparecen, se rechaza y se avisa.
- **Nada se borra:** lo que sobra queda `descartada`.

### 4.6 Búsqueda

- **Híbrida:**
  1. Por palabras, en español y sin acentos.
  2. Por significado, comparando huellas.
  3. Se toman los 50 mejores de cada una y se mezclan por posición (fusión por ranking, k = 60).
  4. Por significado solo cuenta lo que pasa un piso de parecido (0,45 las notas y 0,50 los pedazos de conversación; medido con `bge-m3`, una pregunta que no tiene nada que ver queda por debajo).
  5. Por palabras, una coincidencia es firme si el texto tiene todas las palabras buscadas (siete de cada diez si son más de tres). Un pedazo de conversación que no llega no se muestra; una nota que no llega aparece solo si falta algo mejor, marcada como parcial.
- **Alcance:**
  - Primero el proyecto.
  - Si hay pocos resultados firmes (menos de 3), se amplía a la empresa y después a todo, aclarando de dónde vino cada resultado.
- **Orden:**
  - Notas vigentes, después fragmentos de conversación, después reemplazadas.
  - Aunque sobren notas, quedan hasta 3 lugares para fragmentos.
  - Lo más reciente suma un poco.
- **Velocidad buscada:** menos de 50 ms sin contar la huella de la consulta (unos 20 a 50 ms más con Ollama).
- **Registro:** cada búsqueda deja una señal (consulta y resultados) para detectar fallas.

### 4.7 Lectura de conversaciones

- **Fuente:** `~/.claude/projects/<carpeta>/<id>.jsonl`. Hoy son 8,5 GB en 37 carpetas.
  - Se lee solo el texto del usuario y las respuestas de Claude.
  - Se dejan afuera los resultados de herramientas y los subagentes, que son ruido y la mayor parte del tamaño.
  - El lector tolera cambios de formato entre versiones de Claude Code; si no entiende algo, deja una señal de error.
  - Tampoco entra lo que la app mete en los mensajes del usuario (recordatorios, avisos de tareas, comandos) ni lo pegado desde otro lado.
- **Cuáles se leen:** las conversaciones que registró el mod (son las que tienen un grupo confiable), desde donde había quedado cada una, cuando llevan 2 minutos sin cambios. El historial anterior al mod queda para la etapa 4.
- **Grupo:** el que informó el mod para esa sesión; si no hay, el de la carpeta. Si la conversación cambia de grupo, sus fragmentos van con ella.
- **Fragmentos:** ventanas de unos 1.500 caracteres, con algo de solapamiento. Cada una dice quién habla. Un fragmento que ya está en el grupo con el mismo texto no se guarda de nuevo (una conversación abierta como copia de otra trae repetido todo lo anterior).
- **Nocturno (modelo local):**
  - Corre entre las 2 y las 6, solo con la Mac enchufada y sin uso.
  - Toma cada conversación terminada que no tenga notas de Claude y le pide a un modelo local las notas candidatas en un formato fijo. Pasan por las mismas reglas de calidad.
  - El recorrido inicial de lo viejo se hace así, grupo por grupo, en varias noches.

### 4.8 Circuito de mejora

1. **Detección:**
   - **Claude, en el chat:** llama a `memoria_reportar` con el ejemplo concreto.
   - **El servicio, por señales:** por ejemplo, tres búsquedas vacías parecidas en un grupo, una repetida que pasó el filtro o un error. Crea la propuesta asociada a la conversación donde pasó, y la respuesta de la herramienta que Claude estaba usando avisa que hay una mejora para preguntar.
2. **Firma:**
   - Si el mismo problema ya fue rechazado, no se vuelve a preguntar, salvo que el ejemplo sea distinto.
   - Si ya está en curso, se avisa eso.
3. **Pregunta:** Claude, en ese chat, en una o dos oraciones, aclarando si toca la base, borra datos o suma librerías.
4. **Aceptación** (`memoria_mejorar`): el servicio le pasa el pedido al guardián, que:
   1. Suma el ejemplo del reporte a la batería, antes de que el agente empiece.
   2. Crea una copia del código (`git worktree` en `~/.memoria/trabajo/mejora-<id>`) y una copia de la base para pruebas (`memoria_prueba_<id>`).
   3. Deja el encargo para el agente, que corre dentro de una conversación de Claude, lanzado por el mod (al principio lo lanzaba el guardián con `claude -p` y una llave; ver «El agente corre dentro de Claude», más abajo).
      - **Permisos limitados:** leer y editar solo `crates/servicio/` y `migraciones/`, y compilar y probar.
      - **Sin internet ni otras herramientas:** sin búsqueda web y sin servidores MCP. Internet solo para bajar librerías, si el usuario aceptó una mejora que las suma.
      - **Límite de 60 minutos.**
   4. El agente hace el cambio y deja un resumen.
   5. El guardián compila, corre las pruebas y la batería contra la copia de la base, y compara. Se acepta solo si:
      - el puntaje no baja;
      - no se rompe ninguna pregunta que ya andaba;
      - ninguna pregunta de control trae de la base real notas que antes no traía;
      - pasa la pregunta nueva;
      - el cambio no toca archivos protegidos.
   6. Si pasa, instala de a una mejora por vez:
      1. respaldo de la base;
      2. migraciones;
      3. reemplazo del programa (guardando el anterior);
      4. reinicio;
      5. chequeo de salud durante 30 segundos. Si el chequeo falla, vuelve al programa anterior y restaura la base.
   7. Registra todo en `mejoras` y deja el aviso pendiente.
5. **Aviso y reintento:** lo hace el mod, en el mismo chat (4.4).

**Archivos protegidos (el agente no puede tocarlos):** `crates/guardian/`, `bateria/`, la configuración de arranque y las instrucciones fijas del agente. El guardián rechaza cualquier cambio que los toque.

**Cómo quedó hecho (13/11/2026):**
- **Permisos.** «Toca la base», «borra datos» y «suma librerías» son lo que el reporte dice que el arreglo puede necesitar; el usuario lo acepta al contestar y el guardián lo hace cumplir: sin ese permiso, un cambio con una migración nueva, con un borrado o con librerías nuevas se descarta. Una migración ya aplicada no se puede cambiar nunca.
- **Firma.** Son las palabras de lo que se buscó (o del resumen), sin acentos ni terminaciones. Para las propuestas del servicio, además, no se repite una mientras haya otra abierta por búsquedas parecidas en el mismo grupo.
- **Batería.** Tiene dos partes: la inicial (`bateria/inicial.json`, con sus propias notas, que se cargan en la copia de la base) y la que crece con cada reporte (tabla `bateria`, sobre notas reales). Lo esperado tiene que estar entre los 5 primeros resultados, buscando solo en notas.
- **Preguntas de control.** La batería inicial incluye preguntas que no tienen que traer nada («receta de tarta de manzana»): miden que la búsqueda no invente resultados. Eso solo se puede juzgar con las notas de la batería, que se sabe que no hablan de esos temas. La copia de la base trae además las notas reales, y de esas el guardián no sabe de qué hablan: el día que una nombra el tema de un control, o habla de algo vecino (una nota sobre Docker para «kubernetes»), encontrarla puede estar bien. Por eso un control falla solo si trae una nota de la batería. Lo que trae de la base real no se juzga: se anota (el comando `bateria` lo muestra) y, al comparar, un cambio no puede hacer que un control traiga notas que antes no traía. A los controles se les piden hasta 20 resultados, para comparar bien una corrida con la otra. Antes un control fallaba con cualquier resultado y quedaba fallando para siempre apenas la memoria real nombraba su tema: dejaba de medir. Pasó dos veces el mismo día, con una nota que citaba la frase de un control y con otra que hablaba de un tema parecido.
- **De dónde parte cada mejora.** Del commit del que salió lo instalado (`~/.memoria/instalada`), no de la rama principal. Si lo instalado tenía cambios sin guardar en git, no se hacen mejoras hasta guardarlos e instalar de nuevo.
- **Vuelta atrás.** La base solo se restaura si el programa nuevo llegó a cambiar su estructura. La base que falló se renombra (`memoria_fallida_…`), no se borra.
- **El agente no necesita ninguna llave** desde el 14/11/2026: corre dentro de la conversación de Claude (ver abajo). Antes había que sacarle una con `claude setup-token`.
- **Sin internet** se cumple quitándole al agente las herramientas de red y los servidores MCP; su `cargo` sí tiene red, porque la necesita para compilar, y no hay un encierro del sistema operativo. Lo que el agente escribe en el servicio se ejecuta al probar (pruebas, pasos de compilación): por eso lo que decide qué se instala no es el agente sino el guardián, que revisa qué archivos cambiaron, prueba y mide.
- **Señales.** Se guardan `busqueda`, `busqueda_vacia`, `lenta` y `error`. Quedaron sin hacer `resultado_no_usado` y `repetida`.
- **Probado** con un ensayo completo (copia de la base, del código y del servicio) y un agente de mentira: una mejora buena se instala; una que rompe la batería, una que toca archivos protegidos y una sin cambios se descartan; una instalación que sale mal se deshace con base incluida; y dos a la vez se instalan en fila.

**El agente corre dentro de Claude (14/11/2026).** La primera versión lanzaba al agente desde el guardián, con `claude -p` y una llave que el usuario tenía que sacar y pegar a mano. Era una mala experiencia para algo que se usa conversando con Claude, así que ahora:
- **Tres tramos.** El guardián *prepara* (batería, copia del código, encargo), *espera* y, cuando el agente termina, *revisa, prueba, mide e instala*. Mientras espera no ocupa ningún lugar: los dos lugares del guardián son para preparar y para probar.
- **Fases.** Mientras una mejora está `en_curso`, la columna `fase` dice dónde: `preparando`, `esperando_agente`, `con_agente`, `agente_listo`, `agente_fallo` o `probando` (migración `0002`). Al lado quedan el `encargo` (lo que el guardián le deja al mod: la carpeta de la copia, el `cargo` de la mejora y el pedido), el `informe` (lo último que dijo el agente), de qué versión partió la copia, en qué conversación corre el agente, su última señal y cuántos se lanzaron.
- **El servicio hace de buzón.** `memoria_mejora_estado` le cuenta al mod qué mejoras puede lanzar esa conversación (`para_agente`) y cuáles tienen su agente en ella (`con_agente`); con esa misma herramienta el mod anota los pasos de su agente (`paso`: `empezo`, `termino` con el informe, o `fallo` con el motivo). No hay una herramienta aparte para eso: una conversación que ya estaba abierta cuando se actualizó el servicio no conoce sus herramientas nuevas, y el mod se quedaba sin poder tomar la mejora, en silencio. Una mejora la toma una sola conversación, y hasta dos agentes trabajan a la vez.
- **Lo que le sale mal al mod no se calla.** Si no puede lanzar al agente, se ve con `/memoria` y se lo cuenta al servicio, que lo deja en su registro.
- **Una copia vieja se arma de nuevo.** Si mientras una mejora espera a su agente cambia la versión instalada (se instaló otra mejora, o se reinstaló a mano), el guardián arma la copia otra vez sobre lo instalado antes de que la tome nadie.
- **Quién la lanza.** La conversación donde se aceptó. Si no vuelve a abrirse en seis horas, otra del mismo grupo; una mejora que no quedó atada a ninguna conversación, cualquiera.
- **Los límites los pone el mod**, que es quien ve cada cosa que el agente quiere hacer:
  - El tipo de agente (`memoria:mantenimiento`) tiene solo seis herramientas (leer, buscar por nombre y por contenido, editar, escribir y la terminal), no ve las instrucciones personales del usuario y nunca le pregunta nada: lo que el mod no aprueba, se niega.
  - Antes de cada llamada (`tool.call`): las rutas se resuelven dentro de la copia; leer y buscar, solo ahí; escribir, solo en `crates/servicio/` y `migraciones/`; y de la terminal solo pasa `cargo build|check|test|clippy|fmt` con opciones simples, cambiado por el `cargo` de la mejora.
  - En el permiso (`tool.check`): se aprueba solo lo que ya quedó en esa forma; lo demás se niega. Así la app nunca le pide permiso al usuario por algo del agente.
  - Lo que hace el usuario en la conversación, y cualquier otro agente, sigue de largo sin cambios.
- **El `cargo` de la mejora** es un programita que deja el guardián al lado de la copia (fuera de ella, para que el agente no lo edite): entra a la copia, limpia el entorno (nada de la conversación de Claude pasa a lo que se compila o se prueba) y compila en la carpeta de esa mejora. No se le corta la red: una de las librerías baja una pieza ya compilada la primera vez que se arma en una carpeta nueva, y sin red el servicio no enlaza (pasó en la primera corrida real). Que no se sumen librerías sin permiso lo cuida el guardián al revisar qué cambió.
- **Las reglas del agente siguen en el guardián** (`crates/guardian/instrucciones.md`): viajan al principio del pedido. El agente no las puede cambiar.
- **Si el agente no termina.** El mod avisa cada 10 segundos qué agentes siguen andando. Si deja de avisar dos minutos (se cerró la conversación o la app), si el agente pasa de 60 minutos o si el mod anota que falló, el guardián arma la copia de nuevo y la mejora vuelve a esperar. Al tercer agente que no termina, queda sin instalar y se avisa el motivo. El silencio se mide con el reloj de la Mac despierta: una siesta de la máquina no cuenta.
- **Si el guardián le saca la mejora a un agente que sigue vivo**, el mod se entera en la vuelta siguiente y desde ahí le niega todo, para que termine.
- **Lo que cambia para el usuario:** no hay llave; el agente trabaja solo con la conversación abierta; y se ve entre las tareas de fondo de esa conversación.

**Lo que enseñó la primera corrida real (14 al 16/11/2026).** La primera mejora de verdad lanzó al agente sin llave, pero en la carpeta de la conversación y sin ninguno de sus límites. Se lo detuvo al minuto; solo había intentado leer archivos que no existían y listar una carpeta. Con un mod de prueba (una «sonda»: agentes que solo podían leer un archivo) se vio por qué, y quedó así:
- **Dónde se lanza.** Los ganchos de un mod alcanzan a un agente solo si el mod lo lanzó mientras atendía algo de la conversación. Lanzado después, por su cuenta (desde un reloj), el agente corre fuera de todos los ganchos del mod: no ve sus llamadas ni decide sus permisos; solo se entera de que terminó. Por eso el mod ya no lanza desde el reloj: lo hace adentro del gancho de `memoria_mejorar` (esperando unos segundos a que el guardián arme la copia), al terminar un turno de la conversación, con un mensaje del usuario o al terminar un agente que no pudo. El reloj solo pregunta, avisa y vigila.
- **La espera la hace el servicio.** Adentro de un gancho el mod no puede quedarse dormido, así que `memoria_mejora_estado` acepta `esperar_ms`: contesta apenas la copia está lista, o enseguida si no hay ninguna en camino.
- **El freno.** El mod anota el arranque de cada agente (`turn.start`). Si lanzó uno y no escuchó su arranque, sus límites no lo alcanzan: lo detiene en el acto (con la herramienta de la app para detener tareas) y lo anota como caído. Lo mismo si el mod se recarga con un agente trabajando, o si el guardián le saca la mejora.
- **La carpeta.** La app no respeta la carpeta que pide un mod al lanzar: el agente arranca en la de la conversación. El pedido dice dónde está la copia y el mod completa ahí cada ruta relativa; `cargo` entra solo a la copia.
- **El informe.** El agente lo entrega por una vía propia de la app (`SubagentHandback`), que la app revisa sola. El mod no la toca: si intenta aprobarla él, la app la rechaza.
- **Sin el mod, el agente no escribe.** Con `permissionMode: dontAsk`, lo que el mod no aprueba queda negado: un agente fuera de sus límites puede leer, pero no editar ni correr nada que cambie algo. Es la red de abajo; el freno hace que ni siquiera llegue a leer.
- **Una conversación abierta no conoce las herramientas nuevas del servicio** hasta que se reinicia: lo que el mod necesite del servicio tiene que ir por herramientas que ya existían.

**La primera mejora que recorrió todo el camino (16/11/2026).** Con eso corregido, la mejora 1 se lanzó de nuevo: el servicio esperó 3 segundos a que el guardián armara la copia, el mod lanzó al agente adentro de la llamada que la aceptó, el agente trabajó 72 minutos dentro de sus límites (62 lecturas, 51 ediciones y 25 corridas de `cargo`, sin que la app pidiera un solo permiso), entregó su informe, y el guardián revisó sus cinco archivos y lo probó. **Lo descartó:** al correr las pruebas por su cuenta falló una de las que usan el modelo de verdad, que al agente le había pasado dos veces. El cambio bajaba un umbral hasta quedar a milésimas de lo que da una pregunta que no tiene nada que ver, y los parecidos cambian un poco de una corrida a otra. No se instaló nada. Lo que dejó:
- **El `cargo` del agente necesita red.** Con la red cortada, la librería del modelo no bajaba su pieza y el servicio no enlazaba (ver arriba).
- **El agente trabaja con su propio nivel de esfuerzo** (`effort: high`). Heredaba el de la conversación; con la conversación al máximo, una sola respuesta suya tardó 20 minutos.
- **Tiene 60 minutos**, no 45, y las reglas le piden no leer todo el repositorio.
- **Las reglas avisan que el guardián prueba de nuevo** y que un cambio que dependa del tercer decimal de un umbral se descarta; piden correr todas las pruebas dos veces y no dejar pruebas temporales.
- **El cambio queda guardado aunque no pase.** El guardián lo guarda en la rama de la mejora (`mejora-N-I`, en `~/.memoria/codigo`) antes de compilar y probar, y anota la rama: así se puede ver qué había hecho el agente. Antes se perdía.
- **Queda pendiente:** que una mejora cuyo cambio no pasa las pruebas vuelva al agente con lo que falló, en vez de terminar ahí.

**Los dos descartes eran falsos: el guardián corría con prioridad baja (16/11/2026).** La mejora se lanzó una tercera vez; el agente, ya con su esfuerzo propio, terminó en 10 minutos y medio con otro cambio, y el guardián lo descartó por la misma prueba que antes: la que usa el modelo de verdad. Dos cambios distintos fallando en la misma prueba, y solo en la corrida del guardián, no era casualidad: esa prueba falla sobre el código sin ningún cambio si se la corre con prioridad de fondo (`taskpolicy -b`), que es como launchd arrancaba al guardián. Con esa prioridad macOS manda el modelo a los núcleos lentos, la primera huella no llega dentro de lo que espera una búsqueda y la búsqueda sigue solo por palabras. Quedó así:
- **El guardián arranca con prioridad normal**, como el servicio: corre pruebas y prende copias del servicio para medir, y todo eso usa el modelo.
- **La prueba con el modelo de verdad le da tiempo a cargarse** antes de medir: ya no falla por lenta.
- **El cambio guardado se volvió a probar sin lanzar otro agente:** como el guardián ahora guarda el cambio en su rama antes de probarlo, alcanzó con armar de nuevo la copia desde esa rama y dejar la mejora como «el agente terminó».

**Concurrencia:**
- Hasta 2 agentes a la vez. La instalación se hace en fila.
- Si una mejora quedó vieja porque otra instaló antes y no se puede combinar, el agente la rehace sobre la versión nueva, con un reintento como máximo.

**Los reportes son pedidos, no órdenes.** Pueden venir de conversaciones donde se leyó un mail o una página con texto que intenta dar órdenes. Las instrucciones fijas del agente lo dicen, y sus permisos impiden hacer algo fuera de la copia.

### 4.9 Operación

- **Arranque:** dos agentes de `launchd`, uno para el servicio y otro para el guardián, siempre vivos.
- **Salud:** `GET /salud` en el servicio. El guardián lo usa después de cada instalación.
- **Respaldos:** diarios, 14 días; además, uno antes de cada migración.
- **Consumo:**
  - Cada mejora es un agente de fondo dentro de una conversación de Claude, que usa el plan del usuario.
  - Lo nocturno y las huellas corren en la Mac, sin costo.
- **Actualizar el guardián:** solo a mano, desde una conversación con el usuario. Nunca lo hace el agente.

Estructura del repositorio:

```
personal-memory/
├── crates/
│   ├── servicio/      — lo que usa Claude; el agente lo puede mejorar
│   └── guardian/      — mejoras, batería, instalación y vuelta atrás (protegido)
├── migraciones/       — cambios de la base
├── bateria/           — preguntas iniciales de la batería (protegido)
├── mod/               — el mod "memoria" para Claude
├── instalacion/       — instalador (deja todo en una carpeta aparte), launchd, alta del MCP
└── docs/diseno.md     — este documento
```

### 4.10 Seguridad y privacidad

- Todo queda en la Mac. El servicio escucha solo localmente y pide clave; la base es un archivo que solo puede leer su dueño.
- La única vez que el servicio usa internet es para bajar el modelo de huellas, al instalar. Después corre sin conexión.
- Lo único que sale es lo que ve el agente de mantenimiento al usar Claude: el código y el ejemplo del reporte, que puede incluir partes de notas.
- No se guarda ninguna llave de Claude: el agente usa la sesión de la conversación donde corre.
- Filtro de secretos antes de guardar notas y fragmentos.
- Ningún agente borra datos: solo se descartan.

### 4.11 Estilo del usuario: que Claude escriba como él (propuesta, sin acordar)

Pedido del usuario del 13/11/2026: que la memoria aprenda cómo escribe y que Claude lo use cuando le pide responder un mensaje o mandar un mail.

- **Se elige al instalar:** el instalador pregunta qué fuentes conectar, una por una, y solo lee las que se aceptan. Queda anotado en la configuración y se cambia corriendo el instalador de nuevo.
- **Qué se junta, cada día:** solo los mensajes que escribió el usuario, nunca los de los demás.
  - Chats con Claude: ya los lee el lector.
  - WhatsApp: de la base local de WhatsApp Desktop (`ChatStorage.sqlite`), sobre una copia y en solo lectura, como hace la herramienta `whatsapp-leer`.
  - Slack: los espacios que ya tienen conector en esta Mac.
  - Mails enviados (sugerencia): es la mejor fuente para aprender cómo escribe los mails.
- **Qué se guarda:**
  - Los mensajes propios, con de dónde salieron (lugar, conversación, a quién, idioma, fecha) y su huella. Pasan por el filtro de secretos.
  - Un perfil por lugar e idioma, con dos partes: los números que calcula el programa (largo, signos, mayúsculas, acentos, emojis, saludos y despedidas, muletillas, mezcla de idiomas) y una descripción corta que escribe el modelo local de noche.
- **Cómo lo usa Claude:** una herramienta `memoria_estilo(lugar, idioma?, para?, sobre?)` que devuelve el perfil y los mensajes del usuario más parecidos a la situación (a esa persona, sobre ese tema). El mod suma la regla: antes de escribir algo que va a salir con el nombre del usuario, pedir el estilo.
- **Privacidad:** todo queda en la Mac y el análisis lo hace el modelo local.
- **Preguntas abiertas:** si dentro de una fuente se dejan afuera algunas conversaciones (por ejemplo, chats de familia); cómo se lee Slack sin duplicar las claves que hoy tienen los conectores locales.

### 4.12 Versión 0.4: un archivo y un modelo adentro (13/11/2026)

**Por qué.** Para poder publicarla y que le sirva a otros: instalar Postgres con pgvector y tener Ollama siempre prendido es mucho pedir, y Ollama sin placa gráfica es lento. La memoria tiene que andar con lo que trae.

**La base es un archivo de SQLite** (`~/.memoria/memoria.db`), que el servicio crea solo.

- Las listas (orígenes, etiquetas, carpetas) se guardan como texto en formato JSON; las fechas, como texto en UTC que escribe siempre el programa; un proyecto vacío quiere decir «la empresa en sí».
- **Búsqueda por palabras:** dos tablas de texto de SQLite (FTS5), una para notas y otra para pedazos de conversación. Lo que se guarda ahí no es el texto sino sus palabras reducidas a la raíz, que calcula el programa (`texto.rs`): minúsculas, sin acentos y sin terminación («entregas» y «entregamos» dan «entreg»). Como recortar con y sin acento a veces da distinto («facturación» y «facturacion»), se guardan las dos formas. Si cambia la manera de reducir las palabras, el servicio lo nota al arrancar y arma el índice de nuevo.
- **Preguntas largas:** con hasta tres palabras tienen que estar todas; con más alcanza con siete de cada diez. Para no revisar todos los pedazos que tengan alguna palabra, se empieza por las menos comunes: a un pedazo que cumple le pueden faltar, como mucho, unas pocas, así que de cualquier grupo de «las que pueden faltar más una» tiene al menos una.
- **Búsqueda por significado:** las huellas se guardan en la base y, al arrancar, se cargan todas en memoria (`indice.rs`). Cada búsqueda compara contra las que correspondan, una por una: con decenas de miles tarda milésimas de segundo y el resultado es exacto, sin el índice aproximado que hacía falta en Postgres. Cuesta memoria: unos 300 MB cada 100.000 pedazos con el modelo por defecto.
- **Respaldos:** una copia entera del archivo (`VACUUM INTO`), que se puede hacer con el servicio andando. Volver atrás es poner esa copia en su lugar.
- **El guardián** ya no necesita bases de desarrollo ni permisos: una copia de la base para probar es otro archivo, y la base que falla en una instalación queda al lado con otro nombre.
- **Las pruebas** arman cada una su base en una carpeta temporal: corren en un segundo y sin nada prendido.

**El modelo de huellas corre dentro del servicio** (`interno.rs`). Se baja una vez al instalar y queda en `~/.memoria/modelos/`.

Se midieron cuatro opciones con las mismas notas y preguntas (las de la batería y otras de calibración) en una Mac con procesador M5 Pro:

| Modelo | Tamaño | Batería | Pedazos por segundo | Memoria al trabajar | Cómo distingue lo que no tiene nada que ver |
|---|---|---|---|---|---|
| EmbeddingGemma 300M a 4 bits (`gemma`) | 192 MB | 27 de 27 | 6 | cerca de 1 GB | Bien: una pregunta y su nota dan 0,28 a 0,66; una pregunta ajena, hasta 0,21 |
| EmbeddingGemma 300M a 8 bits | 326 MB | 27 de 27 | 6 | 3,5 GB | Igual que la de 4 bits. Descartada por la memoria |
| EmbeddingGemma 300M sin comprimir | 1,2 GB | no se corrió | 8 | 2,3 GB | Sin medir. Descartada por tamaño y memoria |
| multilingual-e5-small (`e5-small`) | 465 MB | 26 de 27 | 8 | 1,8 GB | Mal: aprieta todo entre 0,80 y 0,93; la nota correcta más floja (0,81) queda por debajo de la ajena más alta (0,84) |
| paraphrase-multilingual-MiniLM comprimido | 257 MB | no se corrió | sin medir | sin medir | Mal, y además ordena peor (16 de 20 primeras). Descartado |
| `bge-m3` (el de la 0.3) | 2,2 GB | 27 de 27 con Ollama | 0,8 adentro; 37 con Ollama y placa gráfica | sin medir adentro | Bien: 0,45 a 0,70 contra 0,44 |

- **Va `gemma` por defecto:** iguala a `bge-m3` en la batería, pesa once veces menos y no necesita placa gráfica. Su licencia es la de Gemma, de Google, no una licencia libre común (el código de este repositorio es MIT; el modelo se baja aparte y no forma parte de él). La alternativa con licencia MIT es `e5-small`, más floja.
- **La memoria fue lo que decidió la variante.** Las tres formas de EmbeddingGemma aciertan lo mismo, pero la de 8 bits llega a 3,5 GB al calcular huellas de pedazos largos y la de 4 bits se queda en 1 GB, a la misma velocidad. Con el modelo soltado (a los 15 minutos sin uso) el servicio instalado queda en unos 220 MB.
- **Cada modelo mide en su escala**, así que los umbrales (unir, avisar, piso de notas, piso de conversaciones) van con el modelo. Con `e5-small` se probó además restarle a cada huella lo que todas tienen en común («centrar»): abre la escala pero no arregla las preguntas difíciles, así que no se usa.
- **Algunos modelos esperan un texto delante** de una pregunta y otro delante de un texto guardado; el servicio lo pone solo según para qué es la huella.
- **De a un texto por vez:** en los modelos comprimidos la huella de un texto cambia según con cuáles otros se calcula, así que no se calculan en tandas. De paso, una búsqueda que llega mientras se leen conversaciones espera, como mucho, un pedazo.
- **Descanso:** a los 15 minutos sin uso el modelo se suelta de la memoria; vuelve en uno o dos segundos.
- **Ollama sigue siendo una opción** (`MEMORIA_HUELLAS=ollama`): es lo más rápido para quien tiene placa gráfica o procesador de Apple.
- Cambiar de modelo obliga a calcular todas las huellas de nuevo. El servicio lo hace solo, de fondo.

**Sin nombres propios (versión 0.5, 14/11/2026).** Para que le sirva a cualquiera:

- **La regla de grupos es general y vive en el servicio** (`grupos.rs`); antes estaba en el mod, con las empresas de quien la escribió. El nombre del grupo en el costado de la app manda («[Empresa] - Proyecto»; un nombre sin corchetes va en «Otras»). Sin grupo del costado, la conversación va al grupo donde se trabajó la última vez en esa carpeta o en la que la contiene; si la carpeta nunca se vio, a «Otras › nombre de la carpeta». Los nombres quedan como se escribieron la primera vez y se reconocen aunque cambien las mayúsculas. El mod solo manda lo que sabe (grupo del costado, carpeta, título) y muestra el grupo que le devuelve el servicio.
- **Una conversación ya registrada no cambia de grupo por llegar sin grupo del costado:** puede ser solo que la app todavía no respondió. Se muda cuando pasa a otro grupo del costado.
- **El nombre de quien la usa es un ajuste** (`MEMORIA_NOMBRE`; por defecto «Usuario»): es como figura en lo que se guarda de sus conversaciones. Los mensajes para Claude dicen «el usuario».
- **Los ejemplos, las pruebas y los documentos usan nombres inventados** (ACME, Portal, Tienda, Globex).
- Los agentes de arranque se llaman `personal-memory.memoria` y `personal-memory.guardian`, y el catálogo del mod, `personal-memory`.

**Lo instalado va aparte del repositorio.** El instalador deja todo en una carpeta propia (`~/.memoria`, o la que diga `MEMORIA_DIR`): los programas, el mod (`catalogo/`, de donde lo lee Claude), una copia propia del código de la versión instalada (`codigo/`) y los datos. Después de instalar nada depende de la carpeta del repositorio. El guardián arma y guarda las mejoras en esa copia del código, a nombre propio, y nunca toca el repositorio donde se desarrolla: una mejora puede traer ejemplos de lo que cada uno buscó, y no tiene que llegar sola a un repositorio que se comparte. Para sumar una mejora al repositorio hay que traerla a mano. Si al reinstalar lo instalado tiene mejoras que el repositorio no trae, el instalador avisa y no sigue.

**Lo que falta para publicarla de verdad:** que solo se instala en macOS (launchd) y que el mod depende de la app de Claude.

## 5. Etapas

| Etapa | Qué incluye | Está lista cuando… |
|---|---|---|
| 1. Base | Base y migraciones; servicio con `contexto`, `buscar` (por palabras) y `guardar`; mod con resumen al empezar; arranque automático; alta del MCP; respaldo diario | En un chat de ACME › Portal, Claude recibe el resumen y guarda una decisión, y en otro chat de Portal la encuentra |
| 2. Significado y calidad | Huellas con `bge-m3`; búsqueda híbrida; unión de repetidas; reemplazos; lector de conversaciones con fragmentos | "el problema del login" encuentra "error de sesión en Cognito", y guardar dos veces lo mismo deja una sola nota |
| 3. Mejoras | Señales; `reportar` y `mejorar`; guardián; batería inicial; agente; aviso y reintento en el chat | Una falla provocada se reporta, se acepta, se instala y el chat retoma solo; y una mejora que rompe la batería se rechaza y todo queda como estaba |
| 4. Lo viejo | Extracción nocturna con el modelo local; recorrido del historial grupo por grupo | El historial de ACME queda procesado y las notas resultantes pasan la batería |
| 5. Después | La regla de grupos pasa al servicio (hecho en la 0.5); gancho de inicio para la terminal; preparar el paso a un servidor | Según se necesite |
| 6. Estilo (propuesta) | Mensajes propios de los chats, WhatsApp y Slack; perfil por lugar e idioma; `memoria_estilo` | Claude redacta un mensaje de WhatsApp y un mail que suenan al usuario usando el perfil |

Hechas: la 1 (12/11/2026) y la 2 (13/11/2026). La 3 está construida, instalada y ensayada con un agente de mentira (13/11/2026); desde el 14/11/2026 el agente corre dentro de la conversación de Claude, sin llave (4.8). El 13/11/2026, además, la versión 0.4 sacó a Postgres y a Ollama de lo que hace falta para instalarla, y el 14/11/2026 la 0.5 sacó los nombres propios y dejó una regla de grupos general (4.12).

## 6. Riesgos y preguntas abiertas

- **Calidad de las notas automáticas sin revisión.** Se mide con la batería y con las señales. Si hay ruido, se suben los umbrales o se restringe qué escribe el modelo local.
- **Peso del modelo local** (unos 18 GB). Solo de noche, con la Mac enchufada y sin uso.
- **Retomar con `$.prompt.submit`.** Hay que verificar en la etapa 3 cómo se comporta si el usuario está escribiendo en ese momento.
- **Id de sesión en las herramientas.** Depende de que el mod lo pase. Sin mod (terminal), el grupo sale por la carpeta.
- **Formato de las conversaciones guardadas.** Puede cambiar entre versiones de Claude Code; el lector tiene que tolerarlo y avisar.
- **Nombre.** "memoria" es provisorio.
