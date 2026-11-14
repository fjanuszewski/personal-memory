// De qué grupo es una conversación lo decide el servicio, con el nombre de
// su grupo en el costado de la app o, si no tiene, con su carpeta. Acá solo
// queda cómo se lee su respuesta y un nombre provisorio para mostrar mientras
// el servicio no respondió. Sin `$`: solo datos.

export const OTRAS = 'Otras'
const SIN_GRUPO = 'Sin grupo'

/** El grupo que informa el servicio en el primer renglón del resumen: «Grupo: Empresa › Proyecto · 3 notas vigentes». */
export function grupoDelResumen(resumen: string): string | null {
  const m = /^Grupo: (.+?) · /.exec(resumen)

  return m?.[1]?.trim() || null
}

/**
 * Cómo nombrar el grupo mientras el servicio no dijo cuál es. Sigue la misma
 * idea que el servicio, sin lo que solo él sabe (en qué grupo se trabajó
 * antes en esa carpeta, y cómo se escribió el nombre la primera vez).
 */
export function provisorio(grupoApp: string | null, carpeta: string): string {
  const grupo = grupoApp?.trim()
  if (grupo) {
    const m = /^\[\s*([^\]]+?)\s*\]\s*(?:[-–—:]\s*)?(.*)$/.exec(grupo)
    if (!m) return `${OTRAS} › ${grupo}`
    const empresa = m[1] ?? ''
    const proyecto = (m[2] ?? '').trim()

    return proyecto && proyecto.toLowerCase() !== 'general' ? `${empresa} › ${proyecto}` : empresa
  }
  const nombre = carpeta.split('/').filter(Boolean).pop()

  return `${OTRAS} › ${nombre ?? SIN_GRUPO}`
}
