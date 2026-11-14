// Mod «memoria»: al empezar cada conversación lee su grupo en la app, la
// registra en el servicio memoria y le pasa a Claude el resumen del grupo y
// las reglas de uso como sección de sus instrucciones (docs/diseno.md, 4.4).
//
// La app arma las instrucciones una sola vez por conversación: lo que el mod
// devuelva después queda guardado hasta que la conversación se resume. Por
// eso lo que cambie a mitad de camino (la conversación pasó a otro grupo del
// costado, la memoria respondió tarde, el mod se cargó con la conversación ya
// empezada) se le adjunta a Claude con el próximo mensaje, una sola vez.
//
// Además sigue las mejoras de la memoria que el usuario aceptó en la conversación:
// cuando una termina, deja el aviso en el chat y, si se instaló, le pide a
// Claude que retome lo que había fallado.
import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, Timer } from 'claude-code'

import type { Avisos, Contexto, Entrega } from '../types'
import { aSesionApp, motivoDe, textoDe, textoSuelto } from './datos'
import type { SesionApp } from './datos'
import { grupoDelResumen, provisorio } from './grupos'
import { leerEstado, retomar } from './mejoras'
import { actualizacion, claveDe, ID_SECCION, seccion } from './seccion'

const SERVIDOR = 'memoria'
const SESIONES = 'ccd_session_mgmt'
// En una conversación nueva la app tarda en conectarse: se espera un poco
// antes de seguir sin ella y usar la carpeta.
const ESPERAS_APP_MS = [1_000, 2_000, 4_000, 8_000]
// Si la memoria no responde, se reintenta en silencio.
const REINTENTOS_MS = [5_000, 15_000, 30_000, 60_000, 120_000]
// Cuánto se espera, como mucho, a la memoria antes de un pedido.
const ESPERA_INSTRUCCIONES_MS = 5_000
// Cuánto se espera, como mucho, a la app al revisar el grupo antes de un mensaje.
const ESPERA_REVISION_MS = 3_000
// Mientras haya una mejora en curso lanzada desde acá, cada cuánto se pregunta por ella.
const CADA_MEJORA_MS = 10_000
const HERRAMIENTA_MEJORAR = 'mcp__memoria__memoria_mejorar'

// Cambiar la etiqueta si cambian los campos (o si lo guardado por una versión
// anterior ya no sirve): descarta lo que sobrevive a la recarga.
const contexto = atom({ plugin: 'memoria', key: 'contexto' } as const, null, {
  shape: 'contexto-3',
})
const SIN_ENTREGA: Entrega = { compuesto: false, clave: null }
const entrega = atom({ plugin: 'memoria', key: 'entrega' } as const, SIN_ENTREGA, {
  shape: 'entrega-2',
})
const SIN_AVISOS: Avisos = { mostrados: [] }
const avisos = atom({ plugin: 'memoria', key: 'avisos' } as const, SIN_AVISOS, {
  shape: 'avisos-1',
})

// ---------- pedidos ----------

// Una herramienta de la app: primero directo al servidor; si no lo alcanza,
// como llamada de herramienta común (igual que el panel de conversaciones).
async function pedirApp(
  $: EngineInterface,
  herramienta: string,
  args: Record<string, unknown>,
): Promise<string> {
  try {
    const r = await $.mcp.call(SESIONES, herramienta, args)
    const texto = textoDe(r)
    if (r.isError) throw new Error(texto || 'la app devolvió un error')

    return texto
  } catch (primero) {
    const r = await $.tool.call({ tool: `mcp__${SESIONES}__${herramienta}`, ...args })
    if (r.deny !== undefined) throw new Error(r.deny)
    const texto = textoSuelto(r.result)
    if (!texto) throw primero

    return texto
  }
}

// El servicio, solo por MCP: como herramienta común podría pedirle permiso
// al usuario en medio del arranque.
async function pedirMemoria(
  $: EngineInterface,
  herramienta: string,
  args: Record<string, unknown>,
): Promise<string> {
  const r = await $.mcp.call(SERVIDOR, herramienta, args)
  const texto = textoDe(r)
  if (r.isError) throw new Error(texto || 'la memoria devolvió un error')

  return texto
}

async function leerApp($: EngineInterface): Promise<SesionApp | null> {
  try {
    return aSesionApp(JSON.parse(await pedirApp($, 'get_session', { session_id: 'self' })))
  } catch {
    return null
  }
}

function dormir($: EngineInterface, ms: number): Promise<void> {
  return new Promise(listo => {
    $.clock.after(ms, () => listo())
  })
}

async function leerAppConEspera($: EngineInterface): Promise<SesionApp | null> {
  let app = await leerApp($)
  for (const ms of ESPERAS_APP_MS) {
    if (app) break
    await dormir($, ms)
    app = await leerApp($)
  }

  return app
}

// La promesa, o nada si tarda más de `ms`.
function conLimite<T>($: EngineInterface, p: Promise<T>, ms: number): Promise<T | undefined> {
  return new Promise(listo => {
    const t = $.clock.after(ms, () => listo(undefined))
    p.then(
      v => {
        t.cancel()
        listo(v)
      },
      () => {
        t.cancel()
        listo(undefined)
      },
    )
  })
}

// ---------- registro ----------

let carpetaInicial = ''
let enCurso: Promise<boolean> | null = null
let reintento: Timer | undefined
// Si se armaron las instrucciones desde el último mensaje que las esperaba.
let compusoDesdeMensaje = true

// Lee el grupo, registra la conversación y guarda el resumen. Con `dada`, usa
// esos datos de la app en vez de volver a pedirlos.
async function preparar($: EngineInterface, dada?: SesionApp | null): Promise<boolean> {
  const transcripcion = await $.session.id().catch(() => '')
  // La app de escritorio le pasa a cada conversación su id; sin él es una
  // terminal y no hay app que esperar.
  const idApp = (await $.env.get('CLAUDE_CODE_HOST_SESSION_ID').catch(() => undefined)) || null
  const app =
    dada !== undefined ? dada : idApp !== null ? await leerAppConEspera($) : await leerApp($)
  const previo = await read($, contexto)
  const carpeta = app?.carpeta || carpetaInicial
  const grupoApp = app?.grupoApp ?? null
  const sesion = app?.id ?? idApp ?? (transcripcion || previo?.sesion || '')
  // El grupo lo decide el servicio; hasta que responda se muestra uno provisorio.
  const sinConfirmar = provisorio(grupoApp, carpeta)
  const base = { sesion, grupoApp, deLaApp: app !== null, enApp: idApp !== null || app !== null }
  if (!sesion) {
    await update($, contexto, () => ({
      ...base,
      grupo: sinConfirmar,
      resumen: null,
      error: 'no sé el id de esta conversación',
    }))

    return false
  }
  try {
    const resumen = await pedirMemoria($, 'memoria_contexto', {
      sesion,
      registro: {
        grupo_app: grupoApp,
        carpeta: carpeta || null,
        titulo: app?.titulo ?? null,
        transcripcion: transcripcion || null,
      },
    })
    const grupo = grupoDelResumen(resumen) ?? sinConfirmar
    await update($, contexto, () => ({ ...base, grupo, resumen, error: null }))

    return true
  } catch (err) {
    await update($, contexto, prev => {
      // Si es la misma conversación en el mismo grupo del costado, lo que ya
      // se sabía (el grupo y su resumen) sirve más que nada.
      const igual = prev !== null && prev.sesion === sesion && prev.grupoApp === grupoApp

      return {
        ...base,
        grupo: igual ? prev.grupo : sinConfirmar,
        resumen: igual ? prev.resumen : null,
        error: motivoDe(err),
      }
    })

    return false
  }
}

function reintentar($: EngineInterface, intento: number): void {
  const ms = REINTENTOS_MS[intento]
  if (ms === undefined) return
  reintento?.cancel()
  reintento = $.clock.after(ms, () => {
    void lanzar($, intento + 1)
  })
}

// ---------- mejoras ----------

let vigia: Timer | undefined
let revisando = false

// Pregunta por las mejoras de esta conversación. Las que terminaron se
// muestran en el chat y, si alguna se instaló, se le pide a Claude que retome
// lo que había fallado. Mientras quede alguna en curso, sigue preguntando.
async function revisarMejoras($: EngineInterface): Promise<void> {
  if (revisando) return
  revisando = true
  try {
    const c = await read($, contexto)
    if (!c || !c.sesion) return
    const pedir = async (vistos: number[]) =>
      leerEstado(await pedirMemoria($, 'memoria_mejora_estado', { sesion: c.sesion, vistos }))
    let { mostrados } = await read($, avisos)
    let estado = await pedir(mostrados)
    // Lo que el servicio ya no devuelve quedó anotado como avisado.
    mostrados = mostrados.filter(id => estado.avisos.some(a => a.id === id))
    const nuevos = estado.avisos.filter(a => !mostrados.includes(a.id))
    for (const aviso of nuevos) {
      const puesto = await $.session.append({
        message: { type: 'system', content: [{ type: 'text', text: aviso.texto }] },
      })
      if (puesto.deny !== undefined) continue
      mostrados = [...mostrados, aviso.id]
      // Se anota antes de pedirle a Claude que retome: si el mod se recarga
      // en el medio, el aviso no se repite.
      const anotados = mostrados
      await update($, avisos, () => ({ mostrados: anotados }))
      // Si Claude está en medio de un turno, el mensaje queda en fila.
      if (aviso.instalada) void $.prompt.submit({ text: retomar(aviso) })
    }
    if (nuevos.length > 0) {
      // Se le confirma al servicio enseguida, sin esperar a la próxima vuelta.
      estado = await pedir(mostrados)
      mostrados = mostrados.filter(id => estado.avisos.some(a => a.id === id))
    }
    const anotados = mostrados
    await update($, avisos, () => ({ mostrados: anotados }))
    if (estado.enCurso.length > 0 || mostrados.length > 0) vigilar($)
    else dejarDeVigilar()
  } catch {
    // El servicio no respondió: se pregunta de nuevo en la próxima vuelta, o
    // la próxima vez que se registre la conversación.
  } finally {
    revisando = false
  }
}

function vigilar($: EngineInterface): void {
  vigia ??= $.clock.every(CADA_MEJORA_MS, () => {
    void revisarMejoras($)
  })
}

function dejarDeVigilar(): void {
  vigia?.cancel()
  vigia = undefined
}

// Una sola preparación a la vez: las instrucciones y los mensajes esperan a esta.
function lanzar($: EngineInterface, intento = 0, app?: SesionApp | null): Promise<boolean> {
  if (enCurso) return enCurso
  reintento?.cancel()
  const p: Promise<boolean> = preparar($, app)
    .then(ok => {
      if (!ok) reintentar($, intento)
      // Con la conversación registrada: avisos que hayan quedado pendientes
      // (de esta conversación, o viejos de otra del mismo grupo).
      else void revisarMejoras($)

      return ok
    })
    .finally(() => {
      if (enCurso === p) enCurso = null
    })
  enCurso = p

  return p
}

// Si la conversación pasó a otro grupo del costado, o la app recién responde,
// se vuelve a registrar.
async function revisarGrupo($: EngineInterface): Promise<void> {
  const previo = await read($, contexto)
  if (!previo || !previo.enApp || enCurso) return
  const app = await leerApp($)
  if (!app) return
  if (previo.deLaApp && app.grupoApp === previo.grupoApp && previo.resumen !== null) return
  await lanzar($, 0, app)
}

function estado(c: Contexto | null, e: Entrega): string {
  if (!c) return 'Memoria: todavía no registré esta conversación.'
  const lineas = [`Memoria: grupo ${c.grupo}, conversación ${c.sesion}.`]
  if (!c.deLaApp) lineas.push('El grupo salió de la carpeta (la app no respondió).')
  lineas.push(c.error === null ? 'El servicio respondió bien.' : `El servicio no respondió: ${c.error}`)
  if (e.clave !== claveDe(c)) lineas.push('Claude recibe esto con tu próximo mensaje.')
  if (c.resumen !== null) lineas.push('', c.resumen)

  return lineas.join('\n')
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const r = await next(e)
    carpetaInicial = e.cwd
    // Con la conversación ya empezada (el mod se cargó o se recargó después),
    // sus instrucciones ya están armadas: la memoria va con el próximo mensaje.
    const turnos = await $.session.turns().catch(() => 0)
    if (turnos > 0) {
      await update($, entrega, prev => (prev.compuesto ? prev : { ...prev, compuesto: true }))
    }
    // Al empezar, y también al recargarse el mod: el resumen se lee de nuevo
    // y, si cambió, Claude lo recibe con el próximo mensaje.
    void lanzar($)
    try {
      await $.command.register({
        name: 'memoria',
        description: 'Muestra el grupo de esta conversación en la memoria y vuelve a leer su resumen',
      })
    } catch {
      // El comando es un atajo; la memoria anda igual sin él.
    }

    return r
  })

  // Al conectarse la app (una conversación nueva que esperaba sin carpeta),
  // se corrige el grupo si había salido de la carpeta.
  on('session.attach', async ($, e, next) => {
    const r = await next(e)
    const previo = await read($, contexto)
    if (!previo || !previo.deLaApp || previo.resumen === null) void lanzar($)

    return r
  })

  on('prompt.compose', async ($, e, next) => {
    const r = await next(e)
    if (e.traits.includes('bare')) return r
    if (enCurso) await conLimite($, enCurso, ESPERA_INSTRUCCIONES_MS)
    const c = await read($, contexto)
    // Una medición (/context) no le manda nada a Claude. De las demás, solo
    // la primera de la conversación le llega: las otras esperan a que se resuma.
    if (!e.traits.includes('analysis')) {
      compusoDesdeMensaje = true
      await update($, entrega, prev => (prev.compuesto ? prev : { compuesto: true, clave: c ? claveDe(c) : null }))
    }
    if (!c) return r
    const resto = r.sections.filter(s => s.id !== ID_SECCION)

    return {
      sections: [...resto, { id: ID_SECCION, text: seccion(c), scope: 'session' as const }],
    }
  })

  // Antes de cada mensaje (del usuario, de otra conversación o un aviso de la
  // app): se mira si la conversación cambió de grupo y, si la memoria cambió
  // desde lo último que recibió Claude, se adjunta.
  on('prompt.submit', async ($, e, next) => {
    if (e.command !== undefined) return next(e)
    await conLimite($, revisarGrupo($), ESPERA_REVISION_MS)
    if (enCurso) await conLimite($, enCurso, ESPERA_INSTRUCCIONES_MS)
    const c = await read($, contexto)
    const ya = await read($, entrega)
    // Todavía no se armaron las instrucciones: las de este pedido la traen.
    if (!ya.compuesto) {
      compusoDesdeMensaje = false

      return next(e)
    }
    if (!c || ya.clave === claveDe(c)) return next(e)
    const texto = ya.clave === null ? seccion(c) : actualizacion(c)
    const clave = claveDe(c)
    const volver = () => update($, entrega, prev => (prev.clave === clave ? { ...prev, clave: ya.clave } : prev))
    // Se marca antes, para que dos mensajes seguidos no la adjunten dos veces.
    await update($, entrega, prev => ({ ...prev, clave }))
    let r
    try {
      r = await next({ ...e, context: [...(e.context ?? []), texto] })
    } catch (err) {
      await volver()
      throw err
    }
    // Si el mensaje no entró, o entró sin lo adjunto, queda para el siguiente.
    if (r.drop !== undefined || !r.context?.includes(texto)) await volver()

    return r
  })

  // Si el primer turno corrió sin que se armaran las instrucciones, lo de la
  // memoria va completo con el próximo mensaje.
  on('turn.complete', async ($, e, next) => {
    const r = await next(e)
    if (e.agentId === undefined && !compusoDesdeMensaje) {
      compusoDesdeMensaje = true
      await update($, entrega, prev => (prev.compuesto ? prev : { compuesto: true, clave: null }))
    }

    return r
  })

  // Al resumir la conversación, lo adjunto a mensajes viejos se puede perder:
  // va de nuevo, completo, con el próximo mensaje.
  on('session.compact', async ($, e, next) => {
    const r = await next(e)
    if (e.agentId === undefined && r.messages !== undefined) {
      await update($, entrega, prev => ({ ...prev, clave: null }))
    }

    return r
  })

  // Un /clear empieza una conversación nueva, con instrucciones nuevas.
  on('session.end', async ($, e, next) => {
    if (e.reason === 'clear') {
      compusoDesdeMensaje = true
      await update($, entrega, () => SIN_ENTREGA)
    }

    return next(e)
  })

  // El usuario aceptó una mejora en esta conversación: se empieza a preguntar por
  // ella hasta que termine.
  on('tool.call', { tool: HERRAMIENTA_MEJORAR }, async ($, e, next) => {
    const r = await next(e)
    if (r.deny === undefined) vigilar($)

    return r
  })

  on('command.run', { command: 'memoria' }, async $ => {
    await lanzar($)

    return { text: estado(await read($, contexto), await read($, entrega)) }
  })
}
