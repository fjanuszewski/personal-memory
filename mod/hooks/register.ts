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
// Antes de cada mensaje mira si Claude le quitó a la conversación la conexión
// con la memoria (le pasa a las que ya estaban abiertas cuando se dio de alta,
// docs/diseno.md 4.3): si pasó, lo deja avisado en el chat y se lo cuenta a
// Claude, para que no trabaje como si la tuviera.
//
// Además sigue las mejoras de la memoria que el usuario aceptó en la conversación:
// cuando una termina, deja el aviso en el chat y, si se instaló, le pide a
// Claude que retome lo que había fallado.
//
// Y es quien pone a trabajar al agente que hace cada mejora: cuando el
// guardián deja lista la copia del código, lanza al agente de fondo dentro de
// esta conversación (con la sesión de Claude que ya está abierta, sin ninguna
// llave), le aprueba solo lo que puede hacer y le niega todo lo demás, sin
// que la app le pregunte nada al usuario, y avisa cuando termina.
//
// Una regla que no se puede romper: al agente se lo lanza siempre desde
// adentro de un gancho que está atendiendo algo de la conversación (la
// llamada que acepta la mejora, el fin de un turno, un mensaje). Los ganchos
// del mod no alcanzan a un agente lanzado fuera de ahí, por ejemplo desde un
// reloj: queda sin límites. Por si igual pasara, hay un freno: si el mod no
// escucha el arranque del agente, lo detiene antes de que haga nada.
import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, Timer } from 'claude-code'

import type { AgenteLanzado, Agentes, Avisos, Contexto, Entrega } from '../types'
import {
  enRegla,
  HERRAMIENTAS,
  INSTRUCCIONES_AGENTE,
  motivoDelCorte,
  NOMBRE_AGENTE,
  revisar,
  SIN_ENCARGO,
  TIPO_AGENTE,
} from './agente'
import type { Encargo } from './agente'
import { aSesionApp, fueSinConexion, motivoDe, motivoDelPedido, textoDe, textoSuelto } from './datos'
import type { SesionApp } from './datos'
import { grupoDelResumen, provisorio } from './grupos'
import { leerEstado, retomar } from './mejoras'
import type { EstadoMejoras } from './mejoras'
import { actualizacion, AVISO_SIN_CONEXION, claveDe, ID_SECCION, seccion } from './seccion'

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
// Con qué entrega un agente su informe final: lo revisa la app, no el mod.
const ENTREGA = 'SubagentHandback'
// Con qué se detiene a un agente.
const FRENO = 'TaskStop'
// Cuánto se le da al guardián para armar la copia cuando se espera adentro de un gancho.
const ESPERA_COPIA_MS = 12_000
const SIN_RESUMEN = 'El agente terminó, pero no quedó registrado su resumen.'

// Cambiar la etiqueta si cambian los campos (o si lo guardado por una versión
// anterior ya no sirve): descarta lo que sobrevive a la recarga.
const contexto = atom({ plugin: 'memoria', key: 'contexto' } as const, null, {
  shape: 'contexto-4',
})
const SIN_ENTREGA: Entrega = { compuesto: false, clave: null }
const entrega = atom({ plugin: 'memoria', key: 'entrega' } as const, SIN_ENTREGA, {
  shape: 'entrega-2',
})
const SIN_AVISOS: Avisos = { mostrados: [] }
const avisos = atom({ plugin: 'memoria', key: 'avisos' } as const, SIN_AVISOS, {
  shape: 'avisos-1',
})
const SIN_AGENTES: Agentes = { lanzados: {} }
const agentes = atom({ plugin: 'memoria', key: 'agentes' } as const, SIN_AGENTES, {
  shape: 'agentes-1',
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

// Si el `$.mcp.connect` de este Claude reconoce la conexión de la memoria
// cuando está. Hasta la 2.1.321 solo conocía las conexiones del propio mod y
// de cualquier otra decía «no listada», estuviera o no. Se averigua en cada
// carga del mod: sabe si alguna vez la da por conectada, y no sabe si dice
// «no listada» justo después de un pedido a la memoria que anduvo. Recién
// cuando sabe, un «no listada» quiere decir que Claude se la quitó a la
// conversación. null: todavía no se sabe.
let conectarSabe: boolean | null = null

// Cómo está la conexión con la memoria según Claude, sin pedirle nada al
// servicio. `otra` es todo lo que acá no se avisa: apagada por el usuario,
// sin poder conectarse, o sin respuesta.
async function conexion($: EngineInterface): Promise<'conectada' | 'quitada' | 'otra'> {
  try {
    const r = await $.mcp.connect(SERVIDOR)
    if (r.isConnected) return 'conectada'

    return r.reason === 'unlisted' ? 'quitada' : 'otra'
  } catch {
    return 'otra'
  }
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
// Si el último registro falló porque Claude no tenía la conexión (y no por el servicio).
let registroSinConexion = false
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
      sinConexion: null,
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
    // Si respondió, la conexión está: lo que se hubiera anotado de antes ya no vale.
    await update($, contexto, () => ({ ...base, grupo, resumen, error: null, sinConexion: null }))
    registroSinConexion = false
    const como = await conexion($)
    if (como === 'conectada') conectarSabe = true
    // La memoria acaba de responder: un Claude que igual dice «no listada» no sabe.
    else if (como === 'quitada') conectarSabe ??= false

    return true
  } catch (err) {
    registroSinConexion = fueSinConexion(err)
    await update($, contexto, prev => {
      // Si es la misma conversación en el mismo grupo del costado, lo que ya
      // se sabía (el grupo y su resumen) sirve más que nada.
      const igual = prev !== null && prev.sesion === sesion && prev.grupoApp === grupoApp

      return {
        ...base,
        grupo: igual ? prev.grupo : sinConfirmar,
        resumen: igual ? prev.resumen : null,
        error: motivoDelPedido(err),
        sinConexion: igual ? prev.sinConexion : null,
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

// ---------- agentes ----------

// Los agentes de la conversación que ya se sabe que no son de la memoria.
const ajenos = new Set<string>()
// Y los que se sabe que sí: solo a estos se les niega todo si el control falla.
const propios = new Set<string>()
// Cuántas veces seguidas no se pudo declarar el tipo de agente al querer lanzar uno, y por qué.
let registrosFallidos = 0
let errorDeRegistro = ''
const REGISTROS_ANTES_DE_AVISAR = 3
// Lo último que salió mal al atender a los agentes: se ve con /memoria y, la
// primera vez, se le cuenta al servicio para que quede en su registro.
let problema = ''
let problemaContado = ''
// Mientras se lanza uno: su primera llamada no puede adelantarse a que quede anotado.
let lanzando: Promise<void> | null = null
let tipoRegistrado = false
// Los agentes cuyo arranque escuchó el mod: es la señal de que sus límites los alcanzan.
const arrancados = new Set<string>()
// Si, por lo último que dijo el servicio, hay alguna mejora esperando que esta conversación lance su agente.
let porLanzar = false
// Un solo lanzamiento a la vez.
let lanzandoPendientes: Promise<void> | null = null
// Agentes que la app dejó de nombrar, o que da por terminados sin que haya
// llegado su fin de turno: se les da una vuelta más antes de soltarlos.
const dudosos = new Set<string>()

const sin = (prev: Agentes, agentId: string): Agentes => {
  const { [agentId]: _, ...resto } = prev.lanzados

  return { lanzados: resto }
}

const con = (prev: Agentes, agentId: string, cambio: Partial<AgenteLanzado>): Agentes => {
  const a = prev.lanzados[agentId]

  return a === undefined ? prev : { lanzados: { ...prev.lanzados, [agentId]: { ...a, ...cambio } } }
}

// Declara el tipo de agente. La app lo acepta recién cuando la conversación
// está armada: si todavía no, se prueba de nuevo antes de lanzar.
async function registrarTipo($: EngineInterface): Promise<boolean> {
  if (tipoRegistrado) return true
  try {
    await $.agent.register({
      name: NOMBRE_AGENTE,
      description:
        'Agente de mantenimiento de la memoria. Lo lanza solo el mod de la memoria cuando el usuario aceptó una mejora: no es para delegarle tareas.',
      prompt: INSTRUCCIONES_AGENTE,
      tools: [...HERRAMIENTAS],
      model: 'inherit',
      // Con su propio nivel de esfuerzo: si heredara el de una conversación
      // puesta al máximo, cada paso suyo podría tardar muchos minutos.
      effort: 'high',
      // Nunca le pregunta nada al usuario: lo que el mod no aprueba, se niega.
      permissionMode: 'dontAsk',
      maxTurns: 200,
      background: true,
      omitClaudeMd: true,
    })
    tipoRegistrado = true
  } catch (err) {
    // Queda para el próximo intento.
    errorDeRegistro = motivoDe(err)
  }

  return tipoRegistrado
}

// De quién es una llamada hecha dentro de un agente: de uno de la memoria
// (con su encargo), de uno de la memoria del que no quedó nada anotado, o de
// un agente que no tiene nada que ver (null).
async function agenteDe($: EngineInterface, agentId: string): Promise<AgenteLanzado | 'sin-encargo' | null> {
  if (ajenos.has(agentId)) return null
  const anotado = async () => (await read($, agentes)).lanzados[agentId]
  let a = await anotado()
  if (a === undefined && lanzando) {
    await lanzando
    a = await anotado()
  }
  if (a !== undefined) {
    propios.add(agentId)

    return a
  }
  let lista
  try {
    lista = await $.agent.list()
  } catch {
    return null
  }
  if (lista.some(x => x.id === agentId && x.type === TIPO_AGENTE)) {
    propios.add(agentId)

    return 'sin-encargo'
  }
  ajenos.add(agentId)

  return null
}

// Le anota al servicio un paso del agente de una mejora. Devuelve si valió.
// Va con la misma herramienta con que el mod pregunta por las mejoras: una
// conversación que ya estaba abierta cuando se actualizó el servicio no
// conoce sus herramientas nuevas.
async function avisarAgente(
  $: EngineInterface,
  sesion: string,
  id: number,
  que: 'empezo' | 'termino' | 'fallo',
  texto?: string,
): Promise<boolean> {
  const r = await pedirMemoria($, 'memoria_mejora_estado', {
    sesion,
    vistos: [],
    paso: { id, que, ...(texto === undefined ? {} : { texto }) },
  })
  const leido: unknown = JSON.parse(r)

  return typeof leido === 'object' && leido !== null && (leido as { paso?: unknown }).paso === true
}

const TERMINADO = ['completed', 'failed', 'killed']

// El agente recién lanzado, buscado entre los de la conversación por su
// descripción: para cuando la app no devuelve su id al lanzarlo.
async function buscarAgente($: EngineInterface, descripcion: string): Promise<string | undefined> {
  const { lanzados } = await read($, agentes)
  const lista = await $.agent.list().catch(() => [])

  return lista.find(
    x =>
      x.type === TIPO_AGENTE &&
      x.description === descripcion &&
      !(x.id in lanzados) &&
      !TERMINADO.includes(x.status),
  )?.id
}

// Detiene a un agente. Es el freno: uno al que no se le pudieron poner los
// límites, o al que el guardián le sacó la mejora, no tiene que seguir.
async function detener($: EngineInterface, agentId: string): Promise<boolean> {
  try {
    const r = await $.tool.call({ tool: FRENO, task_id: agentId } as never)

    return (r as { deny?: string }).deny === undefined
  } catch {
    return false
  }
}

// Toma una mejora para esta conversación y lanza su agente, de fondo. Solo
// se llama desde `lanzarPendientes`, que corre adentro de un gancho.
async function lanzarAgente($: EngineInterface, sesion: string, encargo: Encargo): Promise<void> {
  if (!(await registrarTipo($))) {
    // Se prueba de nuevo en la próxima vuelta. Si no hay caso, se anota como
    // un agente que no arrancó, para que el usuario se entere por el chat.
    registrosFallidos += 1
    problema = `Claude no aceptó el agente de la memoria: ${errorDeRegistro || 'no dijo por qué'}`
    if (registrosFallidos < REGISTROS_ANTES_DE_AVISAR) return
    registrosFallidos = 0
    if (await avisarAgente($, sesion, encargo.id, 'empezo')) {
      const motivo = `Claude no aceptó el agente de la memoria: ${errorDeRegistro || 'no dijo por qué'}`
      await avisarAgente($, sesion, encargo.id, 'fallo', motivo).catch(() => false)
    }

    return
  }
  registrosFallidos = 0
  // Si la tomó otra conversación, o ya hay demasiados agentes, no es para esta.
  if (!(await avisarAgente($, sesion, encargo.id, 'empezo'))) return
  let listo: () => void = () => undefined
  lanzando = new Promise<void>(resolver => {
    listo = resolver
  })
  let motivo: string
  try {
    const descripcion = `Mejora ${encargo.id} de la memoria`
    // La app no deja elegirle la carpeta: trabaja en la de la conversación, y
    // es el mod quien le lleva cada ruta a su copia.
    const r = await $.agent.spawn({
      prompt: encargo.pedido,
      description: descripcion,
      subagentType: TIPO_AGENTE,
    })
    const agentId = r.deny === undefined ? (r.agentId ?? (await buscarAgente($, descripcion))) : undefined
    if (agentId !== undefined && arrancados.has(agentId)) {
      propios.add(agentId)
      await update($, agentes, prev => ({
        lanzados: {
          ...prev.lanzados,
          [agentId]: { mejora: encargo.id, copia: encargo.copia, cargo: encargo.cargo },
        },
      }))
      problema = ''

      return
    }
    if (agentId !== undefined) {
      // El mod no escuchó su arranque: sus límites no lo alcanzan. Se lo
      // frena ya, y queda anotado como anulado por si la app tarda en pararlo.
      propios.add(agentId)
      const frenado = await detener($, agentId)
      await update($, agentes, prev => ({
        lanzados: {
          ...prev.lanzados,
          [agentId]: { mejora: encargo.id, copia: encargo.copia, cargo: encargo.cargo, anulado: true as const },
        },
      }))
      motivo = frenado
        ? 'No pude ponerle los límites al agente: lo detuve antes de que hiciera nada.'
        : 'No pude ponerle los límites al agente ni detenerlo.'
    } else motivo = `Claude no dejó lanzar el agente: ${r.deny ?? 'no arrancó ninguno'}`
  } catch (err) {
    motivo = `No pude lanzar el agente: ${motivoDe(err)}`
  } finally {
    lanzando = null
    listo()
  }
  problema = motivo
  await avisarAgente($, sesion, encargo.id, 'fallo', motivo).catch(() => false)
}

// Anota cómo terminó un agente y se lo avisa al servicio. Si el servicio no
// responde, el aviso queda guardado y sale en la próxima vuelta.
async function terminoAgente(
  $: EngineInterface,
  agentId: string,
  aviso: NonNullable<AgenteLanzado['aviso']>,
): Promise<void> {
  const a = (await read($, agentes)).lanzados[agentId]
  if (a === undefined) return
  dudosos.delete(agentId)
  // Al que el guardián ya le había sacado la mejora no le queda nada por avisar.
  if (a.anulado) {
    await update($, agentes, prev => sin(prev, agentId))

    return
  }
  await update($, agentes, prev => con(prev, agentId, { aviso }))
  const c = await read($, contexto)
  if (!c || !c.sesion) return
  try {
    await avisarAgente($, c.sesion, a.mejora, aviso.paso, aviso.texto)
    await update($, agentes, prev => sin(prev, agentId))
  } catch {
    // El servicio no respondió: se avisa en la próxima vuelta.
  }
  vigilar($)
}

// Lanza los agentes de las mejoras que esperan a esta conversación. Hay que
// llamarla, y esperarla, desde adentro de un gancho que esté atendiendo algo
// de la conversación: es lo que deja al agente al alcance de los límites del
// mod. `esperarMs` es cuánto se le da al guardián para terminar de armar la
// copia (el servicio contesta apenas está lista).
function lanzarPendientes($: EngineInterface, esperarMs = 0): Promise<void> {
  if (lanzandoPendientes) return lanzandoPendientes
  const p = (async () => {
    const c = await read($, contexto)
    if (!c || !c.sesion) return
    const estado = leerEstado(
      await pedirMemoria($, 'memoria_mejora_estado', {
        sesion: c.sesion,
        vistos: [],
        ...(esperarMs > 0 ? { esperar_ms: esperarMs } : {}),
      }),
    )
    const propias = Object.values((await read($, agentes)).lanzados)
      .filter(a => !a.anulado)
      .map(a => a.mejora)
    for (const encargo of estado.paraAgente) {
      if (!propias.includes(encargo.id)) await lanzarAgente($, c.sesion, encargo)
    }
    porLanzar = false
    if (estado.enCurso.length > 0 || estado.paraAgente.length > 0) vigilar($)
  })()
    .catch(err => {
      // Queda a la vista con /memoria y se prueba de nuevo en la próxima ocasión.
      problema = `No pude lanzar al agente de la memoria: ${motivoDe(err)}`
    })
    .finally(() => {
      lanzandoPendientes = null
    })
  lanzandoPendientes = p

  return p
}

// Al cargarse el mod con un agente anotado de antes (se actualizó el mod, o
// la app se reinició): ya no se puede asegurar que sus límites lo alcancen.
// Se lo frena y se anota como caído, para que el guardián arme la copia de
// nuevo y el próximo se lance con los límites puestos.
async function soltarAgentesDeAntes($: EngineInterface): Promise<void> {
  for (const [agentId, a] of Object.entries((await read($, agentes)).lanzados)) {
    // El que ya terminó y solo espera que se avise cómo, sigue su camino.
    if (a.aviso) continue
    await detener($, agentId)
    await terminoAgente($, agentId, {
      paso: 'fallo',
      texto: 'La memoria se recargó mientras el agente trabajaba.',
    })
  }
}

// Lo que toca hacer con los agentes en cada vuelta del reloj: avisar lo que
// quedó sin avisar y soltar a los que ya no corren o ya no tienen la mejora.
// Desde acá no se lanza ninguno: eso va adentro de un gancho.
async function atenderAgentes($: EngineInterface, sesion: string, estado: EstadoMejoras): Promise<void> {
  for (const [agentId, a] of Object.entries((await read($, agentes)).lanzados)) {
    if (!a.aviso) continue
    try {
      await avisarAgente($, sesion, a.mejora, a.aviso.paso, a.aviso.texto)
      await update($, agentes, prev => sin(prev, agentId))
    } catch {
      // Sigue guardado para la próxima vuelta.
    }
  }

  const sinAvisar = Object.entries((await read($, agentes)).lanzados).filter(([, a]) => !a.aviso)
  if (sinAvisar.length > 0) {
    const lista = await $.agent.list().catch(() => null)
    for (const [agentId, a] of sinAvisar) {
      const enLaApp = lista?.find(x => x.id === agentId)
      const andando = enLaApp !== undefined && !TERMINADO.includes(enLaApp.status)
      if (a.anulado) {
        // Se olvida cuando la app ya no lo tiene corriendo; si sigue, se lo frena de nuevo.
        if (lista !== null && !andando) await update($, agentes, prev => sin(prev, agentId))
        else if (andando) await detener($, agentId)
        continue
      }
      // Para el servicio ya no corre acá: el guardián le sacó la mejora.
      if (!estado.conAgente.includes(a.mejora)) {
        await update($, agentes, prev => con(prev, agentId, { anulado: true }))
        await detener($, agentId)
        continue
      }
      if (lista === null || andando) {
        dudosos.delete(agentId)
        continue
      }
      if (enLaApp?.status === 'failed' || enLaApp?.status === 'killed') {
        await terminoAgente($, agentId, { paso: 'fallo', texto: 'El agente se detuvo antes de terminar.' })
        continue
      }
      // Terminó, o la app ya no lo nombra: su fin de turno puede estar por llegar.
      if (!dudosos.has(agentId)) {
        dudosos.add(agentId)
        continue
      }
      await terminoAgente(
        $,
        agentId,
        enLaApp === undefined
          ? { paso: 'fallo', texto: 'El agente dejó de estar en la conversación antes de terminar.' }
          : { paso: 'termino', texto: SIN_RESUMEN },
      )
    }
  }

  // Las que esperan se lanzan en el próximo gancho de la conversación.
  const propias = Object.values((await read($, agentes)).lanzados)
    .filter(a => !a.anulado)
    .map(a => a.mejora)
  porLanzar = estado.paraAgente.some(encargo => !propias.includes(encargo.id))
}

// ---------- mejoras ----------

let vigia: Timer | undefined
let revisando = false

// Pregunta por las mejoras de esta conversación. Las que terminaron se
// muestran en el chat y, si alguna se instaló, se le pide a Claude que retome
// lo que había fallado. De paso avisa qué agentes siguen andando acá y anota
// si alguna mejora espera que se lance el suyo. Mientras quede algo en curso,
// sigue preguntando.
async function revisarMejoras($: EngineInterface): Promise<void> {
  if (revisando) return
  revisando = true
  try {
    const c = await read($, contexto)
    if (!c || !c.sesion) return
    // Los agentes de acá que siguen en marcha: el guardián se entera por este aviso.
    const andando = Object.values((await read($, agentes)).lanzados)
      .filter(a => !a.anulado)
      .map(a => a.mejora)
    const pedir = async (vistos: number[]) => {
      const nuevo = problema !== '' && problema !== problemaContado ? problema : undefined
      const leido = leerEstado(
        await pedirMemoria($, 'memoria_mejora_estado', {
          sesion: c.sesion,
          vistos,
          ...(andando.length > 0 ? { agentes: andando } : {}),
          ...(nuevo === undefined ? {} : { problema: nuevo }),
        }),
      )
      if (nuevo !== undefined) problemaContado = nuevo

      return leido
    }
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
    await atenderAgentes($, c.sesion, estado).catch(err => {
      // No se pierde: queda a la vista con /memoria y se prueba de nuevo en la próxima vuelta.
      problema = `No pude atender al agente de la memoria: ${motivoDe(err)}`
    })
    const conAgentes = Object.keys((await read($, agentes)).lanzados).length > 0
    if (estado.enCurso.length > 0 || mostrados.length > 0 || conAgentes || estado.paraAgente.length > 0) vigilar($)
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

// Antes de cada mensaje: si Claude le quitó la conexión a la conversación,
// queda anotado (Claude se entera con ese mismo mensaje) y el aviso va al
// chat, una vez por cada vez que pasa. Solo vale después de que Claude la
// haya dado por conectada en esta carga del mod: una conexión que todavía se
// está conectando, o que nunca conectó, no es esto. Cuando el mod se recarga
// la conexión vuelve, y `preparar` la da por recuperada.
async function revisarConexion($: EngineInterface): Promise<void> {
  if (conectarSabe === false || enCurso) return
  const previo = await read($, contexto)
  if (!previo) return
  const como = await conexion($)
  if (como === 'conectada') {
    conectarSabe = true
    // Está, y lo último que se supo es que no: volvió sin que el mod se
    // recargara, o el registro falló por eso (el mod se recargó un instante
    // antes que la conexión). Se registra ahora, sin esperar al reintento.
    if (previo.sinConexion !== null || registroSinConexion) {
      await lanzar($)
      await update($, contexto, prev => (prev && prev.sinConexion !== null ? { ...prev, sinConexion: null } : prev))
    }

    return
  }
  if (como !== 'quitada' || conectarSabe !== true || previo.sinConexion !== null) return
  const avisada = await $.session
    .append({ message: { type: 'system', content: [{ type: 'text', text: AVISO_SIN_CONEXION }] } })
    .then(
      puesto => puesto.deny === undefined,
      () => false,
    )
  await update($, contexto, prev => (prev ? { ...prev, sinConexion: { avisada } } : prev))
}

function estado(c: Contexto | null, e: Entrega, a: Agentes): string {
  if (!c) return 'Memoria: todavía no registré esta conversación.'
  const lineas = [`Memoria: grupo ${c.grupo}, conversación ${c.sesion}.`]
  if (!c.deLaApp) lineas.push('El grupo salió de la carpeta (la app no respondió).')
  if (c.sinConexion !== null) lineas.push(AVISO_SIN_CONEXION)
  else lineas.push(c.error === null ? 'El servicio respondió bien.' : `El servicio no respondió: ${c.error}`)
  if (e.clave !== claveDe(c)) lineas.push('Claude recibe esto con tu próximo mensaje.')
  for (const agente of Object.values(a.lanzados)) {
    if (!agente.anulado && !agente.aviso) lineas.push(`Un agente trabaja de fondo en la mejora ${agente.mejora}.`)
  }
  if (problema !== '') lineas.push(problema)
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
    void registrarTipo($)
    void soltarAgentesDeAntes($).catch(() => undefined)
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
    // Si una mejora quedó esperando a su agente, se lo lanza acá, adentro del gancho.
    if (porLanzar) await lanzarPendientes($)
    await conLimite($, revisarGrupo($), ESPERA_REVISION_MS)
    if (enCurso) await conLimite($, enCurso, ESPERA_INSTRUCCIONES_MS)
    await conLimite($, revisarConexion($), ESPERA_REVISION_MS)
    const c = await read($, contexto)
    const ya = await read($, entrega)
    // Todavía no se armaron las instrucciones: las de este pedido la traen.
    if (!ya.compuesto) {
      compusoDesdeMensaje = false

      return next(e)
    }
    if (!c || ya.clave === claveDe(c)) return next(e)
    const texto = ya.clave === null ? seccion(c) : actualizacion(c, ya.clave)
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
    // Terminó un turno de la conversación: buen momento para lanzar al
    // agente de una mejora que quedó lista (acá adentro, para que tenga sus límites).
    if (e.agentId === undefined && (porLanzar || vigia !== undefined)) await lanzarPendientes($)
    // Terminó un agente: si es de la memoria, lo último que dijo es su informe.
    if (e.agentId !== undefined && !ajenos.has(e.agentId)) {
      const informe = e.answer.trim()
      const bien = e.reason === 'answer' && informe !== ''
      const eraDeLaMemoria = (await read($, agentes)).lanzados[e.agentId] !== undefined
      await terminoAgente(
        $,
        e.agentId,
        bien ? { paso: 'termino', texto: informe } : { paso: 'fallo', texto: motivoDelCorte(e.reason) },
      ).catch(() => undefined)
      // Si no terminó bien, el guardián arma la copia de nuevo: se lo espera
      // un momento y el próximo agente se lanza desde acá.
      if (eraDeLaMemoria && !bien) await lanzarPendientes($, ESPERA_COPIA_MS)
    }

    return r
  })

  // El arranque de cada agente. Que el mod lo escuche es la señal de que sus
  // límites lo alcanzan: `lanzarAgente` frena al que arrancó sin que se oyera.
  on('turn.start', ($, e, next) => {
    if (e.agentId !== undefined) arrancados.add(e.agentId)

    return next(e)
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
    if (r.deny === undefined) {
      vigilar($)
      // El agente se lanza acá mismo, mientras se atiende esta llamada: se le
      // dan unos segundos al guardián para armar la copia.
      if (e.agentId === undefined) await lanzarPendientes($, ESPERA_COPIA_MS)
    }

    return r
  })

  // ---------- los límites del agente de mantenimiento ----------

  // Cada cosa que quiere hacer pasa por acá: lo que puede se deja pasar con
  // la ruta completa dentro de su copia (y `cargo` cambiado por el de su
  // mejora); lo demás se niega. Las llamadas de la conversación y de otros
  // agentes siguen de largo.
  on('tool.call', async ($, e, next) => {
    if (e.agentId === undefined) return next(e)
    const a = await agenteDe($, e.agentId)
    if (a === null) return next(e)
    // Su informe final va por una vía de la app, que la app revisa sola.
    if ((e.tool as string) === ENTREGA) return next(e)
    if (a === 'sin-encargo') {
      void detener($, e.agentId)

      return { deny: SIN_ENCARGO }
    }
    const v = revisar(a, e.tool, e as unknown as Record<string, unknown>)
    if ('deny' in v) return { deny: v.deny }

    return next({ ...e, ...v.cambio } as typeof e)
  }).catch(($, e, next) => {
    // Si el control falla, solo se frena a un agente que se sabe que es de la
    // memoria: lo de la conversación y lo de cualquier otro agente sigue.
    if (next.called || e.agentId === undefined || !propios.has(e.agentId)) return next(e)

    return { deny: 'Falló el control de la memoria: no hagas nada más y terminá ahora.' }
  })

  // El permiso: lo que quedó en regla se aprueba sin preguntarle al usuario;
  // cualquier otra cosa del agente se niega, también sin preguntar.
  on('tool.check', async ($, e, next) => {
    if (e.agentId === undefined) return next(e)
    const a = await agenteDe($, e.agentId)
    // El permiso de la entrega del informe lo decide la app: un mod no puede aprobarlo.
    if (a === null || e.tool === ENTREGA) return next(e)
    if (a === 'sin-encargo') return { decision: 'deny' as const, reason: SIN_ENCARGO }
    const args = typeof e.input === 'object' && e.input !== null ? (e.input as Record<string, unknown>) : {}
    const motivo = enRegla(a, e.tool, args)

    return motivo === null ? { decision: 'allow' as const } : { decision: 'deny' as const, reason: motivo }
  }).catch(($, e, next) => {
    if (e.agentId === undefined || !propios.has(e.agentId)) return next(e)

    return { decision: 'deny' as const, reason: 'Falló el control de la memoria.' }
  })

  // Al agente lo lanza el mod: Claude no lo ve entre los que puede usar.
  on('agent.offer', { agent: 'memoria:mantenimiento' }, () => ({ isOffered: false }))

  on('command.run', { command: 'memoria' }, async $ => {
    await lanzar($)
    await revisarConexion($)

    return { text: estado(await read($, contexto), await read($, entrega), await read($, agentes)) }
  })
}
