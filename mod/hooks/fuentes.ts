// Fuentes que se leen por una conexión de Claude (docs/diseno.md, 4.11): un
// espacio de Slack para el que no hay llave en esta máquina, pero que el
// usuario ya tiene conectado a Claude. El servicio no puede usar esa conexión;
// el mod sí, y hace de mensajero: le pregunta al servicio qué pedir, se lo
// pide a Slack por la conexión y le entrega lo que contestó, tal cual.
//
// Acá va lo que no necesita `$`: reconocer la conexión, leer lo que pide el
// servicio y armar el pedido. El mod no mira lo que la conexión contesta, y
// solo le pide dos cosas, las dos de lectura: buscar mensajes de un usuario y
// su perfil. Lo que diga el servicio no puede hacerle pedir otra cosa.

/** Con qué se le buscan mensajes a Slack por la conexión de Claude. */
export const BUSCAR = 'slack_search_public_and_private'
/** Con qué se le pide el perfil de quien está conectado. */
export const PERFIL = 'slack_read_user_profile'
/** Cómo le nombra el mod esa conexión al servicio. */
export const CONEXION_DE_SLACK = 'slack'
// Cuántos mensajes trae la conexión por pedido: es su tope.
const POR_PAGINA = 20
// Entre un pedido y el siguiente se espera lo que diga el servicio, hasta esto.
const PAUSA_MAXIMA_MS = 10_000
const PAUSA_POR_DEFECTO_MS = 1_500

const NOMBRE_DE_LA_BUSQUEDA = /^mcp__(.+)__slack_search_public_and_private$/
const USUARIO = /^[UW][A-Z0-9]{2,20}$/
const SEGUNDOS = /^\d{1,12}$/
const CURSOR = /^[A-Za-z0-9+/=_-]{1,400}$/

/** El nombre de la conexión de Slack de Claude entre las herramientas de la conversación, si está. */
export function conexionDeSlack(herramientas: readonly { name: string }[]): string | null {
  for (const { name } of herramientas) {
    const servidor = NOMBRE_DE_LA_BUSQUEDA.exec(name)?.[1]
    if (servidor) return servidor
  }

  return null
}

/** Lo que el servicio le pide al mod que le traiga de la conexión. */
export type PedidoDeFuente = {
  /** El espacio de trabajo, como lo nombra el usuario en sus ajustes. */
  espacio: string
  /** Cuánto esperar antes del pedido siguiente. */
  pausaMs: number
} & (
  | { pedir: 'perfil' }
  | {
      pedir: 'buscar'
      /** De quién son los mensajes: el usuario de la conexión. */
      usuario: string
      /** Desde qué segundo, o null para pedir desde el principio. */
      despues: string | null
      /** La página que sigue de la misma búsqueda, o null si es la primera. */
      cursor: string | null
    }
)

/**
 * Lee lo que responde el servicio cuando el mod le pregunta por las fuentes.
 * null si no pide nada, o si pide algo que este mod no sabe traer: cada dato
 * tiene que tener la forma esperada, porque va derecho a un pedido.
 */
export function leerPedido(texto: string): PedidoDeFuente | null {
  let crudo: unknown
  try {
    crudo = JSON.parse(texto)
  } catch {
    return null
  }
  const fuente = typeof crudo === 'object' && crudo !== null ? (crudo as { fuente?: unknown }).fuente : null
  if (typeof fuente !== 'object' || fuente === null) return null
  const { espacio, pedir, usuario, despues, cursor, pausa_ms: pausa } = fuente as Record<string, unknown>
  if (typeof espacio !== 'string' || !espacio.trim() || espacio.length > 80) return null
  const pausaMs =
    typeof pausa === 'number' && Number.isFinite(pausa)
      ? Math.min(Math.max(pausa, 0), PAUSA_MAXIMA_MS)
      : PAUSA_POR_DEFECTO_MS
  if (pedir === 'perfil') return { espacio, pedir, pausaMs }
  if (pedir !== 'buscar' || typeof usuario !== 'string' || !USUARIO.test(usuario)) return null
  const desde = despues === null || despues === undefined ? null : despues
  const pagina = cursor === null || cursor === undefined ? null : cursor
  if (desde !== null && (typeof desde !== 'string' || !SEGUNDOS.test(desde))) return null
  if (pagina !== null && (typeof pagina !== 'string' || !CURSOR.test(pagina))) return null

  return { espacio, pedir, usuario, despues: desde, cursor: pagina, pausaMs }
}

/**
 * Los argumentos de la búsqueda: solo los mensajes de ese usuario, del más
 * viejo al más nuevo, sin lo que escribieron los demás alrededor.
 */
export function busqueda(p: PedidoDeFuente & { pedir: 'buscar' }): Record<string, unknown> {
  return {
    filters: `from:<@${p.usuario}>`,
    natural_language_query: '',
    sort: 'timestamp',
    sort_dir: 'asc',
    limit: POR_PAGINA,
    include_context: false,
    response_format: 'detailed',
    ...(p.despues === null ? {} : { after: p.despues }),
    ...(p.cursor === null ? {} : { cursor: p.cursor }),
  }
}
