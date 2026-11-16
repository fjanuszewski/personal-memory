// Mejoras de la memoria (docs/diseno.md, 4.4 y 4.8): lo que el servicio le
// cuenta al mod de una conversación y lo que el mod hace con eso. Sin `$`:
// solo datos.
import { aEncargo } from './agente'
import type { Encargo } from './agente'

/** Una mejora que terminó y todavía no se avisó en el chat. */
export type Aviso = {
  id: number
  /** Si se instaló (true) o no se pudo y todo quedó como estaba (false). */
  instalada: boolean
  /** La línea que se muestra en el chat. */
  texto: string
  /** Lo que se había buscado cuando falló, si el reporte lo traía. */
  buscado: string | null
}

export type EstadoMejoras = {
  /** Las mejoras de esta conversación que siguen andando. */
  enCurso: number[]
  avisos: Aviso[]
  /** Las mejoras que esperan que esta conversación lance su agente. */
  paraAgente: Encargo[]
  /** Las mejoras cuyo agente, para el servicio, corre en esta conversación. */
  conAgente: number[]
  /**
   * Si toca leer las fuentes que el usuario encendió (docs/diseno.md, 4.11):
   * el programa que hay que correr. Si no toca, null.
   */
  juntar: string | null
}

/**
 * El programa que lee las fuentes, si lo que dice el servicio es eso y nada
 * más: la ruta completa de un programa que se llama `memoria`. Lo va a correr
 * el mod, así que no se acepta cualquier cosa.
 */
export function programaParaJuntar(x: unknown): string | null {
  if (typeof x !== 'string') return null
  const partes = x.split('/')
  const completa = x.startsWith('/') && !partes.includes('..') && !partes.includes('.')

  return completa && partes.at(-1) === 'memoria' && x === x.trim() ? x : null
}

const esNumero = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v)

/** Lee lo que responde `memoria_mejora_estado` cuando se le pasa `sesion`. */
export function leerEstado(texto: string): EstadoMejoras {
  const crudo: unknown = JSON.parse(texto)
  if (typeof crudo !== 'object' || crudo === null) throw new Error('respuesta inesperada de la memoria')
  const {
    en_curso: enCurso,
    avisos,
    para_agente: paraAgente,
    con_agente: conAgente,
    juntar,
  } = crudo as { en_curso?: unknown; avisos?: unknown; para_agente?: unknown; con_agente?: unknown; juntar?: unknown }

  return {
    juntar: programaParaJuntar(juntar),
    enCurso: Array.isArray(enCurso) ? enCurso.filter(esNumero) : [],
    paraAgente: (Array.isArray(paraAgente) ? paraAgente : []).flatMap((a: unknown) => aEncargo(a) ?? []),
    conAgente: Array.isArray(conAgente) ? conAgente.filter(esNumero) : [],
    avisos: (Array.isArray(avisos) ? avisos : []).flatMap((a: unknown): Aviso[] => {
      if (typeof a !== 'object' || a === null) return []
      const { id, instalada, texto: linea, buscado } = a as Record<string, unknown>
      if (!esNumero(id) || typeof linea !== 'string' || !linea.trim()) return []

      return [
        {
          id,
          instalada: instalada === true,
          texto: linea.trim(),
          buscado: typeof buscado === 'string' && buscado.trim() ? buscado.trim() : null,
        },
      ]
    }),
  }
}

/** El mensaje para que Claude retome lo que había fallado. */
export function retomar(a: Aviso): string {
  const base = `La memoria instaló la mejora ${a.id}, que el usuario había aceptado en esta conversación.`
  if (a.buscado !== null) {
    return `${base} Volvé a buscar «${a.buscado}» con \`memoria_buscar\` y, si ahora aparece lo que faltaba, seguí con lo que estabas haciendo cuando falló. Si ya no hace falta, no hagas nada más.`
  }

  return `${base} Si lo que estabas haciendo dependía de eso, retomalo; si no, no hagas nada más.`
}
