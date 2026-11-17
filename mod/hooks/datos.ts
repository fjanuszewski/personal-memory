// Leer lo que devuelven la app y el servicio. Sin `$`: solo datos.

export function textoDe(r: {
  content: readonly { type: string; text?: string }[]
}): string {
  return r.content
    .filter(b => b.type === 'text')
    .map(b => b.text ?? '')
    .join('')
}

// Texto de un resultado de herramienta, venga como venga (cadena, bloques u
// objeto con `content`).
export function textoSuelto(x: unknown): string {
  if (typeof x === 'string') return x
  if (Array.isArray(x)) {
    return x
      .map(b =>
        typeof b === 'object' && b !== null && 'text' in b
          ? String((b as { text: unknown }).text)
          : '',
      )
      .join('')
  }
  if (typeof x === 'object' && x !== null && 'content' in x) {
    return textoSuelto((x as { content: unknown }).content)
  }

  return ''
}

export function motivoDe(err: unknown): string {
  return err instanceof Error ? err.message : String(err)
}

// Cuando la conexión con la memoria no está en la conversación, Claude corta
// el pedido antes de que salga, con este texto en inglés.
const SIN_CONECTAR = /no connected MCP tool/i

/** Si un pedido a la memoria falló porque Claude no tiene la conexión, y no por el servicio. */
export function fueSinConexion(err: unknown): boolean {
  return SIN_CONECTAR.test(motivoDe(err))
}

/** Por qué falló un pedido a la memoria, dicho para quien lo lee en /memoria. */
export function motivoDelPedido(err: unknown): string {
  return fueSinConexion(err)
    ? 'Claude no tiene conectada la memoria en esta conversación (puede estar conectándose todavía, o no haber podido)'
    : motivoDe(err)
}

/** Lo que importa de `get_session` de la app. */
export type SesionApp = {
  id: string
  titulo: string | null
  carpeta: string
  grupoApp: string | null
}

export function aSesionApp(x: unknown): SesionApp | null {
  if (typeof x !== 'object' || x === null) return null
  const o = x as Record<string, unknown>
  const id = typeof o.sessionId === 'string' ? o.sessionId : null
  if (!id) return null
  const grupo = o.group as { name?: unknown } | null | undefined

  return {
    id,
    titulo: typeof o.title === 'string' && o.title ? o.title : null,
    carpeta: typeof o.cwd === 'string' ? o.cwd : '',
    grupoApp: typeof grupo?.name === 'string' && grupo.name ? grupo.name : null,
  }
}
