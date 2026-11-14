// El agente de mantenimiento de la memoria (docs/diseno.md, 4.8): qué es y
// qué puede hacer. Corre de fondo dentro de la conversación, lanzado por el
// mod, sobre la copia del código que le armó el guardián; no hace falta
// ninguna llave porque usa la sesión de Claude que ya está abierta.
//
// Acá están sus límites: solo lee y busca dentro de la copia, solo edita el
// servicio y las migraciones, y lo único que corre es el `cargo` que le dejó
// el guardián. Sin `$`: solo datos y reglas.
import type { AgenteLanzado } from '../types'

/** El tipo de agente se llama `<mod>:<nombre>`. */
export const NOMBRE_AGENTE = 'mantenimiento'
export const TIPO_AGENTE = 'memoria:mantenimiento'

/** Lo único que tiene para usar. */
export const HERRAMIENTAS = ['Read', 'Glob', 'Grep', 'Edit', 'Write', 'Bash'] as const

/** Lo que el agente lee antes que nada. Las reglas completas van en su pedido, que las escribe el guardián. */
export const INSTRUCCIONES_AGENTE = `Sos el agente de mantenimiento del servicio «memoria». Trabajás solo y de fondo sobre una copia del código, que es la carpeta donde estás. Nadie te va a contestar: no hagas preguntas.

Tu primer mensaje trae las reglas y el pedido. Seguilas al pie de la letra.

Tus herramientas son leer y buscar dentro de la copia, editar \`crates/servicio/\` y \`migraciones/\`, y \`cargo build\`, \`cargo check\`, \`cargo test\`, \`cargo clippy\` y \`cargo fmt\`, cada uno como un comando solo (sin \`cd\`, sin \`&&\`, sin tuberías, sin redirecciones y sin comillas). Usá rutas relativas a la copia. Todo lo demás se te niega: si algo se te niega, no insistas ni busques un rodeo.`

/** Lo que el guardián deja para una mejora, como llega del servicio. */
export type Encargo = {
  /** El id de la mejora. */
  id: number
  /** La carpeta de la copia del código. */
  copia: string
  /** El `cargo` de esta mejora: lo único que el agente puede correr. */
  cargo: string
  /** Las reglas y el reporte de la falla. */
  pedido: string
}

/** Qué se hace con una llamada del agente: negarla, o dejarla pasar con estos argumentos puestos. */
export type Veredicto = { deny: string } | { cambio: Record<string, unknown> }

const ANULADO = 'Esta tarea se canceló: no hagas nada más y terminá ahora.'
/** Para un agente de la memoria del que no quedó anotado el encargo. */
export const SIN_ENCARGO = 'Esta tarea ya no está en marcha: no hagas nada más y terminá ahora.'
const FUERA = 'Solo podés leer y buscar dentro de la copia del código. Usá rutas relativas a ella.'
const SOLO_SERVICIO = 'Solo podés escribir en `crates/servicio/` y en `migraciones/`, dentro de la copia del código.'
const SOLO_CARGO =
  'Lo único que podés correr es `cargo build`, `cargo check`, `cargo test`, `cargo clippy` o `cargo fmt`, como un comando solo: sin `cd`, sin `&&`, sin tuberías (`|`), sin redirecciones (`>`, `2>&1`) y sin comillas. Ya estás en la carpeta de la copia.'
const SIN_CONTROL = 'La llamada no pasó por el control de la memoria.'

const ORDENES = ['build', 'check', 'test', 'clippy', 'fmt']
// Opciones de `cargo` que lo sacan de la copia o le cambian con qué compila.
const PROHIBIDAS = ['--config', '--manifest-path', '--target-dir', '--path', '-Z', '-C']
const LIMITE_MS = 600_000

/** Una ruta absoluta sin `.`, `..` ni barras de más. */
export function normalizar(ruta: string): string {
  const partes: string[] = []
  for (const parte of ruta.split('/')) {
    if (parte === '' || parte === '.') continue
    if (parte === '..') partes.pop()
    else partes.push(parte)
  }

  return `/${partes.join('/')}`
}

/** La ruta que nombra el agente, que puede ser relativa a su copia. */
function absoluta(copia: string, ruta: string): string {
  return normalizar(ruta.startsWith('/') ? ruta : `${copia}/${ruta}`)
}

function dentro(carpeta: string, ruta: string): boolean {
  return ruta === carpeta || ruta.startsWith(`${carpeta}/`)
}

/** Un texto entre comillas simples para la terminal. */
function entreComillas(texto: string): string {
  return `'${texto.replace(/'/g, `'\\''`)}'`
}

function textoDe(valor: unknown): string | null {
  return typeof valor === 'string' && valor.trim() !== '' ? valor.trim() : null
}

function revisarLectura(copia: string, ruta: unknown, clave: string): Veredicto {
  const dada = textoDe(ruta)
  if (dada === null) return { deny: FUERA }
  const completa = absoluta(copia, dada)

  return dentro(copia, completa) ? { cambio: { [clave]: completa } } : { deny: FUERA }
}

function revisarEscritura(copia: string, ruta: unknown): Veredicto {
  const dada = textoDe(ruta)
  if (dada === null) return { deny: SOLO_SERVICIO }
  const completa = absoluta(copia, dada)
  const permitida = ['crates/servicio', 'migraciones'].some(lugar => {
    const carpeta = `${copia}/${lugar}`

    return completa !== carpeta && dentro(carpeta, completa)
  })

  return permitida ? { cambio: { file_path: completa } } : { deny: SOLO_SERVICIO }
}

// Buscar: en la copia entera si no dice dónde, y nunca con un patrón que salga de ella.
function revisarBusqueda(copia: string, args: Record<string, unknown>, conPatron: boolean): Veredicto {
  if (conPatron) {
    // Ni desde la raíz ni hacia arriba, tampoco escondido entre llaves.
    const patron = textoDe(args.pattern) ?? ''
    if (patron.startsWith('/') || patron.startsWith('~') || patron.includes('..')) return { deny: FUERA }
  }
  if (args.path === undefined || args.path === null || args.path === '') return { cambio: { path: copia } }

  return revisarLectura(copia, args.path, 'path')
}

// `cargo …` pasa a ser el `cargo` de la mejora, que corre en la copia con el
// entorno mínimo. Se acepta también ya cambiado: revisarlo dos veces da lo mismo.
function revisarOrden(a: AgenteLanzado, args: Record<string, unknown>): Veredicto {
  const orden = textoDe(args.command)
  if (orden === null) return { deny: SOLO_CARGO }
  const programa = entreComillas(a.cargo)
  const m = /^(RUST_BACKTRACE=(?:1|full) )?(.*)$/s.exec(orden)
  const rastro = m?.[1] ?? ''
  const resto = m?.[2] ?? ''
  let cola: string
  if (resto === 'cargo' || resto.startsWith('cargo ')) cola = resto.slice('cargo'.length)
  else if (resto === programa || resto.startsWith(`${programa} `)) cola = resto.slice(programa.length)
  else return { deny: SOLO_CARGO }
  const partes = cola.split(' ').filter(parte => parte !== '')
  const [cual, ...opciones] = partes
  if (cual === undefined || !ORDENES.includes(cual)) return { deny: SOLO_CARGO }
  for (const opcion of opciones) {
    if (!/^[A-Za-z0-9_.:=,@+/-]+$/.test(opcion)) return { deny: SOLO_CARGO }
    if (opcion.startsWith('/') || opcion.includes('..') || opcion.includes('=/')) return { deny: FUERA }
    if (PROHIBIDAS.some(p => opcion === p || opcion.startsWith(`${p}=`) || (p === '-Z' && opcion.startsWith(p)))) {
      return { deny: `No podés usar \`${opcion}\` con cargo.` }
    }
  }

  return {
    cambio: {
      command: `${rastro}${programa} ${partes.join(' ')}`,
      // Sin dejarlo de fondo (no tendría cómo leer el resultado) y con todo el tiempo que se puede dar.
      timeout: LIMITE_MS,
      run_in_background: false,
      dangerouslyDisableSandbox: false,
    },
  }
}

/** Decide sobre una llamada del agente, con sus argumentos como los escribió. */
export function revisar(a: AgenteLanzado, herramienta: string, args: Record<string, unknown>): Veredicto {
  if (a.anulado) return { deny: ANULADO }
  switch (herramienta) {
    case 'Read':
      return revisarLectura(a.copia, args.file_path, 'file_path')
    case 'Edit':
    case 'Write':
      return revisarEscritura(a.copia, args.file_path)
    case 'Glob':
      return revisarBusqueda(a.copia, args, true)
    case 'Grep':
      return revisarBusqueda(a.copia, args, false)
    case 'Bash':
      return revisarOrden(a, args)
    default:
      return { deny: `No tenés la herramienta ${herramienta}: solo leer, buscar, editar el servicio y cargo.` }
  }
}

/**
 * Para el permiso: una llamada vale solo si ya pasó por `revisar` y quedó
 * como `revisar` la deja. Devuelve por qué no vale, o null si vale.
 */
export function enRegla(a: AgenteLanzado, herramienta: string, args: Record<string, unknown>): string | null {
  const v = revisar(a, herramienta, args)
  if ('deny' in v) return v.deny
  for (const [clave, valor] of Object.entries(v.cambio)) {
    if (clave === 'timeout') continue
    // Lo que no viene vale como «no»: ni de fondo ni fuera del encierro.
    if (valor === false && (args[clave] === undefined || args[clave] === null)) continue
    if (args[clave] !== valor) return SIN_CONTROL
  }

  return null
}

/** Lee un encargo como llega en `para_agente`. */
export function aEncargo(x: unknown): Encargo | null {
  if (typeof x !== 'object' || x === null) return null
  const { id, encargo } = x as { id?: unknown; encargo?: unknown }
  if (typeof id !== 'number' || !Number.isFinite(id) || typeof encargo !== 'object' || encargo === null) return null
  const { copia, cargo, pedido } = encargo as Record<string, unknown>
  const carpeta = textoDe(copia)
  const programa = textoDe(cargo)
  const texto = textoDe(pedido)
  if (carpeta === null || programa === null || texto === null) return null
  if (!carpeta.startsWith('/') || !programa.startsWith('/')) return null

  return { id, copia: normalizar(carpeta), cargo: normalizar(programa), pedido: texto }
}

/** Por qué no terminó bien un agente, dicho para el usuario. */
export function motivoDelCorte(razon: string): string {
  switch (razon) {
    case 'aborted':
      return 'El agente se interrumpió antes de terminar.'
    case 'refusal':
      return 'Claude no quiso seguir con el arreglo.'
    case 'error':
      return 'El agente se cortó por un error de Claude.'
    default:
      return 'El agente terminó sin decir qué hizo.'
  }
}
