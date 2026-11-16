# personal-memory

Memoria a largo plazo por grupo (empresa › proyecto, como ACME › Portal) para las conversaciones con Claude. Corre de fondo en tu Mac, guarda decisiones, errores resueltos, cambios, acuerdos con el cliente y pendientes en un archivo propio, y Claude la consulta y la completa sola en cada conversación. El diseño completo está en [docs/diseno.md](docs/diseno.md).

**Estado:** versión 0.8. Busca por palabras y por significado, une las notas repetidas, lleva la cuenta de qué nota reemplazó a cuál, lee lo conversado, cuando falla se puede arreglar sola si el usuario acepta, y aprende cómo escribe quien la usa para que Claude redacte en su nombre con su estilo. El recorrido del historial viejo (etapa 4) todavía no está.

**No necesita nada instalado aparte.** Desde la versión 0.4 la base es un solo archivo (`~/.memoria/memoria.db`, SQLite) y el modelo que calcula las huellas de significado corre dentro del servicio. Ya no hacen falta Postgres ni Ollama (Ollama queda como opción para quien lo prefiera).

## Piezas

| Carpeta | Qué es |
|---|---|
| `crates/servicio/` | El servicio en Rust: MCP por HTTP en `127.0.0.1:7337` (con clave local), `GET /salud`, la base (un archivo de SQLite), respaldo diario, huellas de significado (con un modelo propio, o con Ollama), lector de conversaciones y lo que aprende de cómo escribe el usuario. |
| `crates/guardian/` | El guardián: otro programa, que toma las mejoras aceptadas, hace trabajar al agente en una copia, prueba lo que hizo, lo instala y vuelve atrás si algo falla. El agente no lo puede tocar. |
| `bateria/` | Las preguntas iniciales con que el guardián mide cada cambio. El agente tampoco las puede tocar. |
| `migraciones/` | Cambios de la base, numerados. El servicio los aplica al arrancar, con un respaldo antes. |
| `mod/` | El mod `memoria` para Claude: al empezar cada conversación la registra con su grupo y le pasa a Claude el resumen y las reglas de uso. |
| `instalacion/` | El instalador, los agentes de arranque (launchd), el alta del MCP, la baja y el programa que trae los datos de la versión anterior (la que usaba Postgres). |

## El repositorio y lo instalado van separados

En este repositorio hay solo código. Lo que anda en la máquina, y todos los datos de cada uno, quedan en una carpeta aparte: `~/.memoria`, o la que se elija con `MEMORIA_DIR` al instalar. Por eso acá no hay nada personal que esconder de git.

| En la carpeta de instalación | Qué es |
|---|---|
| `bin/` | Los programas (el servicio y el guardián) y el comando para desinstalar. |
| `catalogo/` | El mod, de donde lo lee Claude. |
| `codigo/` | Una copia propia del código de la versión instalada. Ahí el guardián arma y guarda las mejoras automáticas. |
| `memoria.db` | La base. Al lado quedan la clave, los ajustes, los respaldos (`respaldos/`) y el modelo de huellas (`modelos/`). |

Después de instalar, nada depende de la carpeta del repositorio: se puede seguir trabajando en ella, moverla o borrarla, y lo instalado sigue igual. Lo que se cambie en el repositorio recién llega a lo instalado cuando se corre el instalador de nuevo.

**Las mejoras automáticas no vuelven solas al repositorio.** Una mejora puede traer ejemplos de lo que cada uno buscó, así que queda en `codigo/`, en una rama propia. Para sumar una a este repositorio hay que traerla a mano, después de mirarla:

```bash
git pull ~/.memoria/codigo instalada
```

Si al reinstalar lo instalado tiene mejoras que el repositorio no trae, el instalador avisa y no sigue, para no sacarlas del programa sin querer.

## Herramientas que ve Claude

- `memoria_contexto(sesion)`: grupo de la conversación y resumen corto (decisiones vigentes, pendientes, errores, acuerdos, cambios). El mod lo llama solo al empezar.
- `memoria_buscar(consulta, sesion?, grupo?, tipo?, desde?, conversaciones?, reemplazadas?, limite?)`: busca por palabras y por significado, en notas y en pedazos de conversaciones. Empieza por el grupo; si hay pocos resultados amplía a la empresa y después a todo, aclarando de dónde viene cada uno.
- `memoria_guardar(tipo, titulo, cuerpo, sesion?, grupo?, repos?, etiquetas?, reemplaza?)`: guarda una nota. Rechaza claves y tokens; si ya había una que decía lo mismo, las une; si se parece a otra, avisa a cuál; con `reemplaza` deja la vieja como reemplazada.
- `memoria_reemplazar(vieja, nueva)`: deja una nota vigente como reemplazada por otra. La vieja no se borra: queda como historia.
- `memoria_reportar(resumen, detalle, sesion?, buscado?, esperado?, obtenido?, notas_esperadas?, toca_base?, borra_datos?, suma_librerias?)`: registra una falla de la memoria y responde si hay que preguntarle al usuario, o si eso ya está en curso o ya lo rechazó.
- `memoria_mejorar(id)` y `memoria_rechazar(id)`: lo que el usuario contestó.
- `memoria_mejora_estado(id)`: en qué está una mejora.
- `memoria_estilo(lugar?, idioma?, para?, sobre?, ejemplos?, sesion?)`: cómo escribe el usuario (lo medido en sus propios mensajes) y algunos mensajes suyos de ejemplo, para redactar algo que va a salir con su nombre.

Con `sesion` en vez de `id`, `memoria_mejora_estado` es además el canal del mod: le cuenta qué pasa con las mejoras de esa conversación y recibe los pasos del agente que el mod lanzó.

## Mejoras automáticas

1. **Se nota una falla.** La reporta Claude en el chat (`memoria_reportar`, con el ejemplo concreto) o la detecta el servicio solo: tres búsquedas parecidas sin resultados en un grupo dejan una propuesta.
2. **Se le pregunta al usuario**, en ese mismo chat, en una o dos oraciones. Si dice que no, no se vuelve a preguntar por lo mismo.
3. **Si acepta, el guardián prepara el trabajo**, de fondo:
   - suma el ejemplo del reporte a la batería de preguntas;
   - arma una copia del código (`~/.memoria/trabajo/`) a partir de la versión instalada, que está en `~/.memoria/codigo`;
   - deja el encargo: la carpeta de la copia, un `cargo` propio de esa mejora y el pedido con las reglas.
4. **El cambio lo hace un agente dentro de Claude.** El mod de la conversación donde se aceptó lo lanza de fondo, con la sesión de Claude que ya está abierta: no hay que sacar ninguna llave ni configurar nada. Mientras tanto se puede seguir conversando. El mod le pone los límites y decide él cada permiso, sin que la app pregunte nada:
   - lee y busca solo dentro de la copia;
   - edita solo `crates/servicio/` y `migraciones/`;
   - lo único que corre es `cargo build`, `check`, `test`, `clippy` y `fmt`, con el entorno mínimo (nada de la conversación pasa a lo que se compila);
   - no tiene internet ni ninguna otra herramienta.

   El mod lo lanza siempre mientras atiende algo de la conversación (la llamada que acepta la mejora, el fin de un turno, un mensaje): es la única forma de que sus límites alcancen al agente. Y tiene un freno: si no escucha el arranque del agente, lo detiene antes de que haga nada.
5. **Cuando el agente termina, el guardián lo revisa todo:**
   - que no haya tocado nada más, ni cambiado la base, borrado datos o sumado librerías sin que el usuario lo haya aceptado;
   - compila, corre las pruebas y el revisor de código;
   - corre la batería con el programa instalado y con el nuevo, cada uno sobre una copia de la base. El cambio se acepta solo si no rompe ninguna pregunta que andaba, no baja el puntaje, no hace que una pregunta de control traiga notas que antes no traía y arregla el ejemplo del reporte.
6. **Instala de a una:** respaldo de la base, reemplazo del programa (guardando el anterior), reinicio y 30 segundos de vigilancia. Si el servicio no queda sano, vuelve al programa anterior y, si el nuevo había cambiado la base, a la base del respaldo. La base que falló no se borra: queda al lado, como `memoria.db.fallida-…`.
7. **Avisa en el chat** donde el usuario aceptó: una línea con el resultado y, si se instaló, un mensaje para que Claude retome lo que había fallado. Si esa conversación se cerró, el aviso aparece al reabrirla o, pasadas unas horas, en otra del mismo grupo.

Hasta dos mejoras se trabajan a la vez. Si una se instala mientras otra estaba en marcha, la segunda se rehace sobre la versión nueva (una vez).

El agente trabaja mientras la conversación donde se aceptó la mejora esté abierta. Si se cierra antes de que termine, la mejora queda esperando: el guardián arma la copia de nuevo y el agente arranca otra vez cuando se vuelve a escribir en esa conversación (o, si pasan unas horas, en otra del mismo grupo). Un agente que no termina se lanza hasta tres veces; después la mejora queda sin instalar, con el motivo a la vista en el chat. Usa el plan de Claude del usuario, como cualquier otra tarea de fondo de la conversación.

Cada mejora parte del commit de git del que salió lo instalado (`~/.memoria/instalada`). Si se instaló con cambios sin guardar en git, las mejoras quedan en pausa hasta guardarlos y correr el instalador de nuevo. Lo que hace cada mejora queda en su rama (`mejora-N-…`) dentro de `~/.memoria/codigo`, y la rama `instalada` de esa copia se adelanta a la versión nueva. El repositorio donde se desarrolla no se toca.

## Aprende cómo escribís

Para que un mensaje o un correo redactado por Claude suene a vos y no a Claude. La memoria junta **solo lo que escribiste vos**, mide cómo lo escribís y se lo cuenta a Claude cuando tiene que redactar algo en tu nombre. No hay ningún modelo de por medio y nada sale de tu máquina: son cuentas sobre tus propios mensajes, guardados en la misma base.

**De dónde aprende.** De tus conversaciones con Claude (las que el servicio ya lee): las repasa solo, de fondo, y la primera vez recorre también lo anterior. Y, si la encendés, de WhatsApp. Las fuentes que no son Claude son siempre optativas: cada una se enciende aparte y, apagada, no se lee.

**WhatsApp (optativa).** A Claude le escribís rápido; para que un mensaje suene a vos tiene que aprender de tus mensajes. Se enciende al instalar:

```bash
MEMORIA_FUENTES=whatsapp instalacion/instalar.sh
# dejando afuera los chats que tengan estos textos en el nombre:
MEMORIA_FUENTES=whatsapp MEMORIA_WHATSAPP_SIN="Familia;Los del colegio" instalacion/instalar.sh
```

- La lee el servicio, solo: un minuto después de arrancar y después cada 6 horas. Trabaja siempre sobre una copia de la base local de WhatsApp Desktop, que vive unos segundos y se borra. La base de la aplicación no se toca.
- Entran solo los mensajes de texto que mandaste vos, en chats con personas y en grupos. No entran los de los demás, las fotos ni los audios, los estados, los canales, lo que reenviaste (lo que la aplicación marca como reenviado, y lo que habías recibido antes palabra por palabra) ni los chats que dejaste afuera.
- De cada chat queda su nombre y una marca. Los números de teléfono no se guardan: un contacto sin agendar queda sin nombre.
- **macOS puede preguntarte** si dejás que «memoria» acceda a los datos de otras apps. Ese permiso dura mientras el programa siga andando, así que lo puede volver a preguntar cuando el servicio arranca de nuevo (al reiniciar la Mac o al actualizar la memoria). Mientras no contestes, la lectura queda esperando y no se pide otra. Si decís que no, se vuelve a intentar al otro día y en el próximo arranque. Si Claude te pide el estilo mientras tanto, la memoria le avisa que está esperando tu permiso.
- **Cómo le escribís a cada uno.** Si Claude dice a quién le vas a escribir y ya le mandaste 30 mensajes o más, mide cómo le escribís a esa persona o a ese grupo, que no siempre es como le escribís al resto. Los saludos se cuentan donde empieza una charla (después de 6 horas sin escribirle), no en cada mensaje.
- Lo que le mandaste a alguien se muestra como ejemplo **solo cuando Claude dice a quién le vas a escribir** y ya le habías escrito: nunca aparecen mensajes de un chat en otro. El nombre tiene que ser el del chat, entero o con palabras enteras («Ana» es «Ana Trabajo», no «Mariana»); si coincide con más de un chat, no elige: los nombra y no muestra mensajes de ninguno.
- Para ver qué entraría sin guardar nada: `~/.memoria/bin/memoria juntar --probar`. Para leer ahora, sin esperar al servicio: `~/.memoria/bin/memoria juntar`. Para apagarla: `MEMORIA_FUENTES=ninguna instalacion/instalar.sh`.

**Qué deja afuera.** En una conversación con Claude no todo lo que figura como tuyo lo escribiste vos, así que se queda nada más con lo que es claramente tuyo y, ante la duda, corta:

- La respuesta que diste tocando un botón: el texto lo había escrito Claude.
- Lo que copiaste de la respuesta anterior para preguntar por eso.
- Lo que te escribió otra persona y pegaste («mirá lo que me dicen:»), los mensajes copiados de una mensajería o de un chat de trabajo, lo que viene después de un cambio de idioma, las tablas y las fichas de datos.
- Los avisos de la app, lo que mandan otras conversaciones y los bloques de código o de registros.
- Las claves y los tokens, que se tapan igual que en el resto de la memoria.

**Qué mide.** Por lugar (dónde escribís) e idioma: el largo de los mensajes, si empezás con mayúscula, si ponés punto final, si abrís las preguntas con «¿», cuántas tildes ponés (y qué palabras escribís sin tilde), si tratás de vos o de tú, los espacios alrededor de los signos, los emojis, con qué palabras arrancás, qué expresiones repetís y qué palabras en inglés metés. Cada dato va con su cuenta al lado («casi nunca (5 de 690)»), y con pocos mensajes dice que es apenas una pista.

**Cómo lo usa Claude.** Antes de redactar algo que va a salir con tu nombre llama a `memoria_estilo` diciendo dónde va a salir. Recibe lo medido y algunos mensajes tuyos de ejemplo: frases cortas y limpias, sin direcciones ni nada que parezca un dato, primero las del grupo de esa conversación (las de otro grupo solo completan si faltan, y dicen de cuál son). Si le pide el estilo de un lugar del que todavía no hay mensajes, usa lo que tiene y lo avisa. A vos te sigue hablando como siempre.

## Cómo busca y cómo cuida la calidad

- **Dos búsquedas a la vez.** Por palabras (en español, sin importar acentos ni terminaciones: «entregas» encuentra «entregamos») y por significado: cada texto tiene una «huella», una lista de números que calcula un modelo dentro del servicio, y dos textos que dicen lo mismo con otras palabras tienen huellas cercanas. Así «el problema del login» encuentra «error de sesión en Cognito». Se mezclan los mejores de cada una; lo que aparece en las dos queda primero.
- **Una palabra suelta no alcanza.** Un resultado vale si llega por significado o si tiene (casi) todas las palabras buscadas. Un pedazo de conversación que solo comparte una palabra no se muestra; una nota así aparece solo cuando no hay nada mejor, marcada como «coincide solo en parte».
- **Repetidas.** Al guardar se compara con las notas vigentes del grupo. Si una dice lo mismo (muy parecida y del mismo tipo), se unen: queda una sola, con el texto nuevo y los dos orígenes. Si solo se parece, se guarda y Claude recibe el aviso. Nunca se unen dos notas que cambian un número, una fecha, un día, un mes o un nombre propio («la factura de octubre» y «la de noviembre» son dos hechos).
- **Reemplazadas.** La búsqueda muestra la nota que vale hoy y, debajo, «antes se usaba» con la anterior. Buscar lo viejo lleva a lo nuevo.
- **Lo conversado.** Cada minuto el servicio lee lo nuevo de las conversaciones que registró el mod (los archivos de `~/.claude/projects`) y lo guarda en pedazos de unos 1.500 caracteres. Entra solo lo que escribió el usuario y lo que respondió Claude; quedan afuera los resultados de herramientas, los subagentes, los avisos de la app y lo pegado desde otro lado. Las claves y los tokens se tapan.
- **Si el modelo de huellas no está disponible** (no se pudo bajar, o Ollama está apagado), todo sigue andando por palabras, y las huellas que falten se completan solas cuando vuelve.

Cuánto parecido hace falta para cada cosa (los umbrales) depende del modelo: cada uno mide en su propia escala. Están en `crates/servicio/src/interno.rs` (los de `bge-m3`, en `estado.rs`) y salen de medir cada modelo con las mismas notas y preguntas en español.

## El modelo de huellas

Por defecto corre dentro del servicio: se baja una sola vez al instalar y después no usa internet. Se puede cambiar por otro. Medido en una Mac con procesador M5 Pro, con la batería de 27 preguntas:

| Opción | Qué baja | Batería | Pedazos de conversación por segundo | Una pregunta | Memoria al trabajar |
|---|---|---|---|---|---|
| `gemma` (por defecto) | 192 MB | 27 de 27 | 7 | unos 15 ms | cerca de 1 GB |
| `e5-small` | 465 MB | 26 de 27 | 8 | unos 12 ms | 1,8 GB |
| `bge-m3` adentro | 2,2 GB | no medida (es el mismo modelo que con Ollama) | menos de 1 | unos 37 ms | sin medir |
| Ollama con `bge-m3` | 1,2 GB, en Ollama | 27 de 27 | 37, con placa gráfica | unos 12 ms | la pone Ollama |

- `gemma` es EmbeddingGemma 300M, de Google, comprimido a 4 bits. Es el que mejor resultado da entre los livianos y el que menos memoria ocupa. Su licencia es la de Gemma (permite usarlo, con las condiciones de Google), no una licencia libre común.
- `e5-small` (licencia MIT) ordena bien, pero distingue peor lo que no tiene nada que ver: por eso se le escapa alguna pregunta dicha con otras palabras.
- Ollama conviene a quien ya lo tiene andando con placa gráfica o con un procesador de Apple: es lo más rápido para leer mucho historial de una vez.
- El modelo interno usa la mitad de los núcleos (hasta 8), para no molestar; `MEMORIA_HILOS` lo cambia. En la Mac donde se midió no gana nada con más de 6.
- **Memoria:** con el modelo cargado el servicio ocupa cerca de 1 GB (820 MB medidos mientras calculaba huellas). A los 15 minutos sin uso el modelo se suelta y el servicio queda en unos 220 MB; vuelve a cargarse solo, en uno o dos segundos, cuando hace falta.

Para cambiar, al instalar:

```bash
MEMORIA_HUELLAS=ollama ./instalacion/instalar.sh
```

```bash
MEMORIA_MODELO_HUELLAS=e5-small ./instalacion/instalar.sh
```

La elección queda en `~/.memoria/ajustes` (renglones `MEMORIA_…=valor`, los mismos nombres que las variables de entorno). Al cambiar de modelo las huellas guardadas dejan de servir y el servicio las calcula todas de nuevo, de fondo; mientras tanto busca por palabras.

Para medir la búsqueda en cualquier momento, sin tocar nada (prende una copia del servicio sobre una copia de la base):

```bash
~/.memoria/bin/memoria-guardian bateria
```

Algunas preguntas de la batería son de control: preguntan por temas de los que sus notas de ejemplo no hablan, y no tienen que traer ninguna de ellas. Si traen notas tuyas, el comando las muestra y no cuenta como falla (puede que hablen de eso); lo que sí se controla es que un cambio no haga que traigan más.

## De qué grupo es cada conversación

No hay nombres de empresas ni de proyectos escritos en el programa: los grupos salen de cómo cada uno ordena sus conversaciones. Lo decide el servicio (`crates/servicio/src/grupos.rs`) con lo que el mod le cuenta:

1. **Si la conversación está en un grupo del costado de la app, ese nombre manda.** «[Empresa] - Proyecto» es esa empresa y ese proyecto; «[Empresa]» es la empresa en sí; cualquier otro nombre va como proyecto de «Otras».
2. **Si no está en ningún grupo del costado, la carpeta:** va al grupo donde se trabajó la última vez en esa carpeta, o en la que la contiene.
3. **Si esa carpeta nunca se vio,** a «Otras › nombre de la carpeta».

Los nombres quedan como se escribieron la primera vez y después se reconocen aunque cambien las mayúsculas. Una conversación ya registrada no cambia de grupo por quedarse sin grupo del costado: solo se muda si pasa a otro.

En lo que se guarda de las conversaciones, quien usa la memoria figura como «Usuario». Para que figure con su nombre, al instalar:

```bash
MEMORIA_NOMBRE=Ana ./instalacion/instalar.sh
```

## Cómo le llega la memoria a Claude

Al empezar cada conversación, el mod suma a sus instrucciones la sección «Memoria del grupo»: las reglas de uso, el id de la conversación y el resumen del grupo. La app arma esas instrucciones una sola vez. Si después la conversación pasa a otro grupo del costado, o el mod se carga con la conversación ya empezada, el mod se lo adjunta a Claude con el próximo mensaje, una sola vez. El comando `/memoria` muestra en qué grupo quedó la conversación.

## Instalar o actualizar

Hace falta Rust para compilar (`brew install rustup`, después `rustup default stable`) y Claude Code. Nada más: ni base de datos ni Ollama.

```bash
./instalacion/instalar.sh
```

Todos los comandos de este documento se corren parado en la carpeta del repositorio.

Compila el servicio y el guardián, los copia a `~/.memoria/bin/` (guarda el servicio anterior como `memoria.anterior`), crea la base (`~/.memoria/memoria.db`), baja el modelo de huellas si falta (192 MB, una sola vez, a `~/.memoria/modelos/`), deja en `~/.memoria/codigo` la versión del código que se instaló, instala los agentes `personal-memory.memoria` y `personal-memory.guardian` para que arranquen con la sesión de macOS, espera a que el servicio responda, da de alta el MCP `memoria` para todas las conversaciones, copia el mod a `~/.memoria/catalogo` y lo da de alta en Claude. Se puede correr las veces que haga falta. El guardián solo se actualiza así, a mano.

Para instalar en otra carpeta:

```bash
MEMORIA_DIR=/otra/carpeta ./instalacion/instalar.sh
```

Si la Mac usó la versión anterior (la que guardaba todo en Postgres), el instalador trae por su cuenta, la primera vez, las notas, las conversaciones leídas y lo demás al archivo nuevo. De Postgres solo lee: lo de allá queda como estaba. Las huellas no se traen: se calculan de nuevo con el modelo nuevo, de fondo.

Claude lee el mod de `~/.memoria/catalogo`, que es una copia de `mod/` hecha por el instalador. Para que un cambio del mod llegue a las conversaciones: subir `version` en `mod/.claude-plugin/plugin.json` y correr el instalador de nuevo. Las conversaciones abiertas lo recargan solas en unos segundos; si no, con `/reload-plugins`.

Para sacarlo (la base, los respaldos y el modelo quedan en `~/.memoria`):

```bash
~/.memoria/bin/desinstalar.sh
```

## Operación

- Salud: `curl http://127.0.0.1:7337/salud`. `huellas` dice si la última huella se pudo calcular (`null` si todavía no hubo ninguna), `modelo_huellas` con qué modelo, y `pendientes` cuántas notas y pedazos de conversación esperan la suya.
- Registros: `~/Library/Logs/memoria/` (uno por día, 14 días; `launchd.log` para lo que pase antes de arrancar). Los del guardián son `guardian.<fecha>.log`: ahí queda cada paso de cada mejora y qué preguntas de la batería fallaron.
- Respaldos: `~/.memoria/respaldos/` (uno por día, 14 días; uno antes de cada migración y uno antes de instalar cada mejora). Cada respaldo es la base entera en un archivo: para volver a ese momento se apaga el servicio y se copia el respaldo en el lugar de `~/.memoria/memoria.db` (borrando, si están, `memoria.db-wal` y `memoria.db-shm`).
- Mejoras: la tabla `mejoras` de la base tiene cada una con su estado, el motivo si falló, qué cambió, de qué versión a cuál y el puntaje de la batería antes y después.
- Clave local: `~/.memoria/clave` (solo legible por el usuario). La usa el alta del MCP.
- Para mirar adentro: `sqlite3 ~/.memoria/memoria.db`, o el archivo abierto en DBeaver o cualquier visor de SQLite.

## Desarrollo

No hace falta nada prendido para compilar ni para probar: cada prueba arma su propia base en una carpeta temporal.

```bash
cargo test                      # dos pruebas usan un modelo de verdad si está a mano (el interno ya bajado
                                # en ~/.memoria/modelos, o bge-m3 en Ollama); si no, se saltean solas
cargo clippy --all-targets      # tiene que quedar sin avisos
```

Las consultas a la base son texto común dentro del código (no se revisan al compilar): por eso cada una tiene que estar cubierta por una prueba. Las palabras de cada nota y de cada pedazo de conversación se guardan ya reducidas a su raíz (`crates/servicio/src/texto.rs`); si cambia esa forma de reducirlas, el servicio arma de nuevo el índice de palabras al arrancar.

Para probar el guardián de punta a punta sin tocar lo instalado se arma un ensayo: una copia de la base (cualquier respaldo sirve), un clon del repositorio, el servicio en otro puerto y el guardián apuntando a todo eso con `MEMORIA_DIR`, `MEMORIA_REPO`, `MEMORIA_SERVICIO`, `MEMORIA_REINICIO` y `MEMORIA_APAGADO`. El agente se imita a mano: se cambia algo en la copia de la mejora y se anota en la base lo que anotaría el mod (`fase = 'agente_listo'` y un `informe`).

El mod se prueba con el Claude Code de la app (2.1.321 o más nuevo; el de la terminal puede ser más viejo). Está dentro de `~/Library/Application Support/Claude/claude-code/`:

```bash
claude plugin test mod
```

## Licencia

MIT: se puede usar, copiar, cambiar y compartir libremente, sin garantías. El texto completo está en [LICENSE](LICENSE).

El modelo de huellas que se baja por defecto (EmbeddingGemma, de Google) no es parte de este repositorio y tiene su propia licencia, la de Gemma. Quien prefiera un modelo con licencia MIT puede elegir `e5-small` o `bge-m3` (ver «El modelo de huellas»).
