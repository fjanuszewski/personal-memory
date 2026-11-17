import { expect, mock, test } from 'claude-code/testing'
import type { On, PromptComposeInput, PromptComposeResult, PromptSubmitInput } from 'claude-code'

import { aEncargo, enRegla, normalizar, revisar } from '../hooks/agente'
import { motivoDelPedido } from '../hooks/datos'
import { busqueda, BUSCAR, conexionDeSlack, leerPedido, PERFIL } from '../hooks/fuentes'
import { grupoDelResumen, provisorio } from '../hooks/grupos'
import type { AgenteLanzado } from '../types'

type Llamada = { server: string; tool: string; args: Record<string, unknown> }

const RESUMEN = [
  'Grupo: ACME › Portal · 1 nota vigente',
  'Decisiones vigentes:',
  '- [nota 1] 2026-11-12 · Usar Cognito para la sesión',
].join('\n')

const texto = (t: string, isError = false) => ({
  value: { content: [{ type: 'text', text: t }], isError },
})

type Registro = { grupo_app?: string | null; carpeta?: string | null } | undefined

// Lo que haría el servicio con lo que le manda el mod. El nombre del costado
// manda, y queda como se guardó la primera vez («[ACME] - PORTAL» es
// «ACME › Portal»). Sin grupo del costado, el grupo donde se trabajó antes
// en esa carpeta.
function grupoSegunElServicio(registro: Registro): string {
  const delCostado = registro?.grupo_app ?? null
  const m = /^\[(.+?)\]\s*-\s*(.+)$/.exec(delCostado ?? '')
  if (m) {
    const proyecto = m[2] ?? ''

    return `${m[1]} › ${proyecto.charAt(0).toUpperCase()}${proyecto.slice(1).toLowerCase()}`
  }
  if (delCostado) return `Otras › ${delCostado}`

  return (registro?.carpeta ?? '').includes('/tienda') ? 'ACME › Tienda' : 'ACME › Portal'
}

type Mundo = {
  /** El grupo del costado en cada lectura; null: sin grupo. */
  grupo?: () => string | null
  /** Sin app de escritorio (una terminal): ni sus sesiones ni su variable. */
  sinApp?: boolean
  /** Cuántas veces falla la lectura de la app antes de responder (recién conectada). */
  fallasApp?: number
  /** Cuántas veces falla la memoria antes de responder. */
  fallasMemoria?: number
  /** Cuántos mensajes tenía la conversación cuando se cargó el mod. */
  turnos?: number
  /**
   * Cómo tiene Claude la conexión con la memoria en cada momento: `bien`,
   * `quitada` (ya no está entre las suyas) o `apagada` (la apagó el usuario).
   * Sin esto, bien.
   */
  conexion?: () => 'bien' | 'quitada' | 'apagada'
  /** Un Claude que solo reconoce las conexiones del propio mod: de la memoria dice siempre «no listada». */
  claudeViejo?: boolean
  /** Algo se niega a dejar avisos del mod a la vista en el chat. */
  sinAvisos?: boolean
  /** Cuántos registros se cortan porque la conexión todavía no está, aunque Claude ya la dé por conectada enseguida. */
  sinConexionAlRegistrar?: number
  /** Las mejoras de la conversación, como las lleva el servicio. La prueba las va cambiando. */
  mejoras?: Mejoras
  /**
   * Lo que hace la app al lanzar un agente desde adentro de un gancho: avisar
   * su arranque a los ganchos del mod. Sin esto, el agente arranca sin que el
   * mod lo escuche, como cuando se lo lanza desde un reloj.
   */
  alArrancar?: (agentId: string) => Promise<unknown>
  /** Lo que hay para traer por la conexión de Slack de Claude. Sin esto, la conversación no la tiene. */
  fuentes?: Fuentes
}

type Fuentes = {
  /** Lo que el servicio le va pidiendo al mod, en orden: cada pregunta gasta uno. Sin más, no pide nada. */
  pedidos: Record<string, unknown>[]
  /** Si la conexión de Slack contesta con un error. */
  falla?: boolean
}

// Como figura en Claude la conexión de Slack: con un nombre que elige la app.
const CONECTOR = 'conector-7f3a'
const PERFIL_DE_ANA = JSON.stringify({ result: 'User ID: U0ANA\nUsername: ana\nReal Name: Ana Ruiz\n' })
const PAGINA_DE_ANA = JSON.stringify({
  results:
    '# Search Results for: \n\n## Messages (1 results)\n### Result 1 of 1\nChannel: #equipo-acme (ID: C0EQUIPO)\nFrom: Ana Ruiz <ana@example.com> (ID: U0ANA) \nMessage_ts: 1794312000.000100\nText: \nalguien pudo revisar lo de ayer?\n\n---\n\n',
  pagination_info: '',
})

type Mejoras = {
  enCurso: number[]
  avisos: { id: number; instalada: boolean; texto: string; buscado: string | null }[]
  /** Las que el guardián dejó listas para que esta conversación lance su agente. */
  paraAgente?: { id: number; encargo: { copia: string; cargo: string; pedido: string } }[]
  /** Las que, para el servicio, tienen su agente corriendo en esta conversación. */
  conAgente?: number[]
  /** Cuántas veces falla el servicio al anotar un paso del agente. */
  fallasAlAnotar?: number
  /** Si Claude se niega a lanzar al agente, con qué motivo. */
  noLanza?: string
  /** Si Claude no acepta el tipo de agente, con qué motivo. */
  noRegistra?: string
}

type Lanzado = { prompt: string; description?: string; subagentType?: string; cwd?: string }

type Visto = {
  /** Lo que se le pidió a la app y a la memoria. */
  llamadas: Llamada[]
  /** Lo que llegó adjunto a cada mensaje. */
  adjuntos: (readonly string[] | undefined)[]
  /** Las líneas que el mod dejó a la vista en el chat. */
  avisos: string[]
  /** Los mensajes que el mod le mandó a Claude para que retome. */
  retomados: string[]
  /** Los tipos de agente que el mod declaró. */
  tipos: Record<string, unknown>[]
  /** Los agentes que el mod lanzó. */
  lanzados: Lanzado[]
  /** Los agentes que la app dice tener, con su tipo, su tarea y cómo están. */
  enLaApp: { id: string; type: string; status: string; description: string }[]
  /** Las herramientas que llegaron a correr, como quedaron después de pasar por el mod. */
  corridas: Record<string, unknown>[]
  /** Los permisos que el mod no decidió y le dejó a la app: de quién y para qué herramienta. */
  consultados: string[]
  /** Los agentes que el mod detuvo. */
  frenados: string[]
  /** Lo que el mod le pidió a la conexión de Slack de Claude. */
  aSlack: { tool: string; args: Record<string, unknown> }[]
  /** Lo que el mod le llevó al servicio al preguntarle por las fuentes. */
  entregas: Record<string, unknown>[]
}

// Un mensaje con este texto no entra (como si otro gancho lo rechazara).
const RECHAZADO = 'rechazado'

// La app (sus sesiones), el servicio memoria y el motor, con lo que se les pidió.
function mundo(on: On, o: Mundo = {}): Visto {
  const visto: Visto = {
    llamadas: [],
    adjuntos: [],
    avisos: [],
    retomados: [],
    tipos: [],
    lanzados: [],
    enLaApp: [],
    corridas: [],
    consultados: [],
    frenados: [],
    aSlack: [],
    entregas: [],
  }
  let fallas = o.fallasMemoria ?? 0
  let cortes = o.sinConexionAlRegistrar ?? 0
  let fallasApp = o.fallasApp ?? 0
  mock.env(on, o.sinApp ? {} : { CLAUDE_CODE_HOST_SESSION_ID: 'local_1' })
  on('command.register', () => ({ value: { command: 'memoria' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.id', () => ({ value: 'cli-123' }))
  on('session.turns', () => ({ value: o.turnos ?? 0 }))
  on('session.attach', (_, e) => ({ clientId: e.clientId }))
  on('session.compact', (_, e) => ({ messages: e.messages }))
  on('turn.complete', (_, e) => ({ text: e.answer }))
  on('turn.start', (_, e) => ({ turnId: e.turnId }))
  on('tool.call', (_, e) => {
    if (e.tool === 'mcp__memoria__memoria_mejorar') return { result: 'Mejora aceptada.' }
    const llamada = e as unknown as Record<string, unknown>
    // El freno: la app detiene al agente.
    if (e.tool === 'TaskStop') {
      const id = String(llamada.task_id)
      visto.frenados.push(id)
      for (const a of visto.enLaApp) if (a.id === id) a.status = 'killed'

      return { result: { message: `Successfully stopped task: ${id}` } }
    }
    if (!['Read', 'Edit', 'Write', 'Glob', 'Grep', 'Bash', 'SubagentHandback'].includes(e.tool)) {
      return { deny: 'sin herramienta' }
    }
    visto.corridas.push(llamada)

    return { result: 'hecho' }
  })
  // Lo que el mod no decide llega hasta la app, que acá deja pasar todo.
  on('tool.check', (_, e) => {
    visto.consultados.push(`${e.agentId ?? 'la conversación'}: ${e.tool}`)

    return { decision: 'allow' as const }
  })
  on('agent.register', (_, e) => {
    if (o.mejoras?.noRegistra !== undefined) return { deny: o.mejoras.noRegistra } as never
    visto.tipos.push(e as unknown as Record<string, unknown>)

    return { value: { agent: `memoria:${e.name}` } }
  })
  on('agent.offer', () => ({ isOffered: true }))
  on('agent.list', () => ({ value: visto.enLaApp as never }))
  // Lo que lanza un mod llega acá como lo recibe la herramienta de agentes
  // de Claude. El id lo pone la app de verdad: en una prueba no vuelve con
  // la respuesta, y el mod lo encuentra en la lista de agentes.
  on('agent.spawn', async (_, e) => {
    const noLanza = o.mejoras?.noLanza
    if (noLanza !== undefined) return { deny: noLanza }
    const pedido = e as unknown as { prompt: string; description: string; subagent_type: string; cwd?: string }
    visto.lanzados.push({
      prompt: pedido.prompt,
      description: pedido.description,
      subagentType: pedido.subagent_type,
      cwd: pedido.cwd,
    })
    const agentId = `a${visto.lanzados.length}`
    visto.enLaApp.push({ id: agentId, type: pedido.subagent_type, status: 'running', description: pedido.description })
    await o.alArrancar?.(agentId)

    return { model: 'inherit', agentId }
  })
  // La fila la guarda el motor: acá solo se mira qué dejó el mod.
  on('session.append', (_, e, next) => {
    if (o.sinAvisos) return { deny: 'acá no se dejan avisos' }
    const [bloque] = e.message.content
    visto.avisos.push(bloque?.type === 'text' ? String(bloque.text) : '')

    return next(e)
  })
  on('prompt.compose', () => ({
    sections: [{ id: 'intro', text: 'Sos Claude.', scope: 'shared' as const }],
  }))
  on('prompt.submit', (_, e) => {
    if (e.origin?.kind === 'plugin') {
      visto.retomados.push(e.text)

      return { text: e.text }
    }
    visto.adjuntos.push(e.context)
    if (e.text === RECHAZADO) return { drop: 'no entra' }

    return { text: e.text, context: e.context }
  })
  // Las herramientas de la conversación: las de la memoria y, si la tiene, las de Slack.
  on('tool.list', () => ({
    value: [
      { name: 'mcp__memoria__memoria_buscar', description: 'Busca en la memoria.', mcp: true },
      ...(o.fuentes
        ? [
            { name: `mcp__${CONECTOR}__slack_send_message`, description: 'Manda un mensaje.', mcp: true },
            { name: `mcp__${CONECTOR}__${BUSCAR}`, description: 'Busca mensajes.', mcp: true, isDeferred: true },
          ]
        : []),
    ] as never,
  }))
  const conexion = () => (o.conexion ? o.conexion() : 'bien')
  // Lo que contesta Claude cuando un mod le pregunta por una conexión.
  on('mcp.connect', (_, e) => {
    if (o.claudeViejo || conexion() === 'quitada') {
      return {
        value: {
          isConnected: false as const,
          reason: 'unlisted' as const,
          message: `Neither this plugin's manifest nor /mcp lists an MCP server named "${e.server}".`,
        },
      }
    }
    if (conexion() === 'apagada') {
      return {
        value: { isConnected: false as const, reason: 'disabled' as const, message: `"${e.server}" is turned off.` },
      }
    }

    return { value: { isConnected: true as const, server: e.server } }
  })
  on('mcp.call', (_, e) => {
    const llamada = e as unknown as Llamada
    // Sin la conexión el pedido no llega al servicio: Claude lo corta antes, con este texto.
    const sinConexion = () => texto(`no connected MCP tool "${llamada.tool}" on a server named "memoria"`, true)
    if (llamada.server === 'memoria' && conexion() !== 'bien') return sinConexion()
    if (llamada.server === 'memoria' && llamada.tool === 'memoria_contexto' && cortes > 0) {
      cortes -= 1

      return sinConexion()
    }
    // La conexión de Slack de Claude: contesta lo que le pidan, sin mirar quién.
    if (llamada.server === CONECTOR) {
      visto.aSlack.push({ tool: llamada.tool, args: llamada.args })
      if (o.fuentes?.falla) return texto('rate_limited', true)

      return texto(llamada.tool === PERFIL ? PERFIL_DE_ANA : PAGINA_DE_ANA)
    }
    // El mod haciendo de mensajero: el servicio recibe lo que trae y le dice qué pedir.
    if (llamada.server === 'memoria' && llamada.tool === 'memoria_mejora_estado' && llamada.args.fuente !== undefined) {
      visto.entregas.push(llamada.args.fuente as Record<string, unknown>)

      return texto(JSON.stringify({ fuente: o.fuentes?.pedidos.shift() ?? null }))
    }
    visto.llamadas.push(llamada)
    if (llamada.server === 'ccd_session_mgmt') {
      if (o.sinApp) return texto('No existe el servidor ccd_session_mgmt', true)
      if (fallasApp > 0) {
        fallasApp -= 1

        return texto('la app todavía no se conectó', true)
      }
      const grupo = o.grupo ? o.grupo() : '[ACME] - PORTAL'

      return texto(
        JSON.stringify({
          sessionId: 'local_1',
          title: 'Tickets Portal',
          cwd: '/Users/ana/GITHUB/acme/portal',
          group: grupo === null ? null : { id: 'g1', name: grupo },
        }),
      )
    }
    if (llamada.server === 'memoria' && llamada.tool === 'memoria_contexto') {
      if (fallas > 0) {
        fallas -= 1

        return texto('memoria apagada', true)
      }
      return texto(RESUMEN.replace('ACME › Portal', grupoSegunElServicio(llamada.args.registro as Registro)))
    }

    if (llamada.server === 'memoria' && llamada.tool === 'memoria_mejora_estado') {
      const mejoras = o.mejoras ?? { enCurso: [], avisos: [] }
      // Lo que el mod dice que ya mostró queda anotado y no vuelve.
      const vistos = (llamada.args.vistos as number[] | undefined) ?? []
      mejoras.avisos = mejoras.avisos.filter(a => !vistos.includes(a.id))

      // Un paso del agente viaja con la misma pregunta. Como el servicio:
      // la mejora la toma quien llega primero, y solo esa conversación dice
      // cómo terminó su agente.
      const paso = llamada.args.paso as { id: number; que: string } | undefined
      let valio: boolean | null = null
      if (paso !== undefined) {
        if ((mejoras.fallasAlAnotar ?? 0) > 0) {
          mejoras.fallasAlAnotar = (mejoras.fallasAlAnotar ?? 0) - 1

          return texto('memoria apagada', true)
        }
        const lista = (mejoras.paraAgente ?? []).some(a => a.id === paso.id)
        const suya = (mejoras.conAgente ?? []).includes(paso.id)
        valio = true
        if (paso.que === 'empezo' && lista) {
          mejoras.paraAgente = (mejoras.paraAgente ?? []).filter(a => a.id !== paso.id)
          mejoras.conAgente = [...(mejoras.conAgente ?? []), paso.id]
        } else if (paso.que !== 'empezo' && suya) {
          mejoras.conAgente = (mejoras.conAgente ?? []).filter(m => m !== paso.id)
        } else valio = false
      }

      return texto(
        JSON.stringify({
          en_curso: mejoras.enCurso,
          avisos: mejoras.avisos,
          para_agente: mejoras.paraAgente ?? [],
          con_agente: mejoras.conAgente ?? [],
          paso: valio,
        }),
      )
    }

    return texto('herramienta desconocida', true)
  })

  return visto
}

const registros = (v: Visto) =>
  v.llamadas.filter(l => l.server === 'memoria' && l.tool === 'memoria_contexto')
const deLaApp = (v: Visto) => v.llamadas.filter(l => l.server === 'ccd_session_mgmt')
// Todo lo que llegó al servicio de la memoria.
const alServicio = (v: Visto) => v.llamadas.filter(l => l.server === 'memoria')
const aLaMemoria = (v: Visto) =>
  v.llamadas.filter(l => l.server === 'memoria' && l.tool === 'memoria_mejora_estado')
// Las preguntas de cada vuelta por las mejoras de la conversación (sin los
// pasos del agente ni la espera por la copia, que van por la misma herramienta).
const porMejoras = (v: Visto) =>
  aLaMemoria(v).filter(l => l.args.paso === undefined && l.args.esperar_ms === undefined)
// Lo que el mod le anotó al servicio sobre sus agentes: va con la misma
// herramienta, que es la que conoce cualquier conversación ya abierta.
const pasos = (v: Visto) =>
  aLaMemoria(v).flatMap(l => {
    const paso = l.args.paso as { id: number; que: string; texto?: string } | undefined
    if (paso === undefined) return []
    const { id, que, ...resto } = paso

    return [{ id, sesion: l.args.sesion, paso: que, ...resto }]
  })

// Lo que el motor le pasa al armar las instrucciones de un pedido.
const PEDIDO: PromptComposeInput = {
  model: 'claude-opus-5-5',
  promptModel: 'claude-opus-5-5',
  surfaces: ['desktop'],
  tools: [],
  outputStyle: null,
  traits: [],
}

type Componer = (e: PromptComposeInput) => Promise<PromptComposeResult>

async function seccion(componer: Componer) {
  const { sections } = await componer(PEDIDO)

  return { sections, memoria: sections.find(s => s.id === 'memoria:contexto') }
}

const INICIO = { cwd: '/Users/ana/GITHUB/acme/portal', surface: 'desktop', isInteractive: true } as const

// Un mensaje que el usuario escribe en la conversación.
const mensaje = (text: string): PromptSubmitInput => ({ text, wait: false, origin: { kind: 'composer' } })
// Un mensaje de otra conversación, que el usuario no ve escrito.
const deOtraConversacion = (text: string): PromptSubmitInput => ({
  text,
  wait: false,
  isMeta: true,
  origin: { kind: 'peer-send-message' },
})
const finDeTurno = { answer: 'listo', durationMs: 10, isAborted: false, turnId: 't1', reason: 'answer' } as const
// El comando /memoria, escrito por el usuario.
const COMANDO = {
  command: 'memoria',
  args: '',
  origin: { kind: 'composer' },
  presentation: { isFullscreen: false, columns: 80 },
} as const

test('registra la conversación con el grupo del costado y suma la sección', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  const visto = mundo(on)

  await $.session.start(INICIO)
  await clock.settle()

  const [registro] = registros(visto)
  expect(registro?.args).toEqual({
    sesion: 'local_1',
    registro: {
      grupo_app: '[ACME] - PORTAL',
      carpeta: '/Users/ana/GITHUB/acme/portal',
      titulo: 'Tickets Portal',
      transcripcion: 'cli-123',
    },
  })

  const { sections, memoria } = await seccion(e => $.prompt.compose(e))
  expect(sections[0]?.id).toBe('intro')
  expect(sections.at(-1)?.id).toBe('memoria:contexto')
  expect(memoria?.scope).toBe('session')
  expect(memoria?.text).toContain('Esta conversación es del grupo «ACME › Portal» y su id es `local_1`')
  expect(memoria?.text).toContain('select:mcp__memoria__memoria_buscar,mcp__memoria__memoria_guardar')
  expect(memoria?.text).toContain('Nunca guardes claves, contraseñas, tokens ni datos personales de terceros.')
  // Y la regla del estilo: pedirlo antes de redactar algo en nombre del usuario.
  expect(memoria?.text).toContain('pedí cómo escribe con `memoria_estilo`')
  expect(memoria?.text).toContain(',mcp__memoria__memoria_estilo».')
  expect(memoria?.text).toContain(RESUMEN)

  // La sección no cambia de un pedido a otro y el servicio se consulta una vez.
  const otra = await seccion(e => $.prompt.compose(e))
  expect(otra.memoria?.text).toBe(memoria?.text)
  expect(registros(visto).length).toBe(1)

  // Claude ya la tiene en sus instrucciones: el mensaje va sin adjuntos.
  await $.prompt.submit(mensaje('hola'))
  expect(visto.adjuntos).toEqual([undefined])
})

test('sin la app usa la carpeta y el id del archivo, sin esperar', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  const visto = mundo(on, { sinApp: true })

  await $.session.start({ cwd: '/Users/ana/GITHUB/acme/tienda/api', surface: 'terminal', isInteractive: true })
  await clock.settle()

  const [registro] = registros(visto)
  expect(registro?.args).toEqual({
    sesion: 'cli-123',
    registro: {
      grupo_app: null,
      carpeta: '/Users/ana/GITHUB/acme/tienda/api',
      titulo: null,
      transcripcion: 'cli-123',
    },
  })
  const { memoria } = await seccion(e => $.prompt.compose(e))
  expect(memoria?.text).toContain('del grupo «ACME › Tienda» y su id es `cli-123`')

  // En la terminal no hay grupo del costado que revisar en cada mensaje.
  const antes = deLaApp(visto).length
  await $.prompt.submit(mensaje('hola'))
  expect(deLaApp(visto).length).toBe(antes)
})

test('en la app, si todavía no se conectó, la espera y usa su grupo', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  const visto = mundo(on, { fallasApp: 2 })

  await $.session.start({ cwd: '/', surface: 'desktop', isInteractive: true })
  await clock.settle()
  expect(registros(visto).length).toBe(0)

  await clock.advance(4_000)
  const [registro] = registros(visto)
  expect(registro?.args.sesion).toBe('local_1')
  expect((registro?.args.registro as { grupo_app: string }).grupo_app).toBe('[ACME] - PORTAL')
})

test('si la memoria no responde lo dice, reintenta y le pasa el resumen después', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  const visto = mundo(on, { fallasMemoria: 1 })

  await $.session.start(INICIO)
  await clock.settle()
  const antes = await seccion(e => $.prompt.compose(e))
  expect(antes.memoria?.text).toContain('No pude leer el resumen del grupo (memoria apagada)')
  expect(antes.memoria?.text).not.toContain(RESUMEN)
  // Mientras el servicio no responde, el grupo se nombra como está en el costado.
  expect(antes.memoria?.text).toContain('del grupo «ACME › PORTAL»')

  await clock.advance(6_000)
  expect(registros(visto).length).toBe(2)

  // Las instrucciones ya estaban armadas: el resumen va con el próximo mensaje.
  await $.prompt.submit(mensaje('seguimos'))
  const [adjunto] = visto.adjuntos[0] ?? []
  expect(adjunto).toContain('Actualización de la memoria')
  // Ahora sí con el nombre que le da el servicio.
  expect(adjunto).toContain('del grupo «ACME › Portal»')
  expect(adjunto).toContain(RESUMEN)
})

test('si la conversación pasa a otro grupo, la vuelve a registrar y se lo avisa a Claude una vez', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  let grupo: string | null = null
  const visto = mundo(on, { grupo: () => grupo })

  await $.session.start(INICIO)
  await clock.settle()
  // Sin grupo en el costado manda la carpeta: el servicio sabe dónde se trabajó antes ahí.
  expect((await seccion(e => $.prompt.compose(e))).memoria?.text).toContain('«ACME › Portal»')

  // El usuario la arrastra a Agenda y escribe.
  grupo = '[ACME] - Agenda'
  await $.prompt.submit(mensaje('seguimos'))
  expect(registros(visto).length).toBe(2)
  expect((registros(visto)[1]?.args.registro as { grupo_app: string }).grupo_app).toBe('[ACME] - Agenda')
  const [adjunto] = visto.adjuntos[0] ?? []
  expect(adjunto).toContain('esta conversación ahora es del grupo «ACME › Agenda» y su id es `local_1`')
  expect(adjunto).toContain(RESUMEN.replace('Portal', 'Agenda'))

  // El aviso va una sola vez; con el mismo grupo no se vuelve a registrar.
  await $.prompt.submit(mensaje('otra cosa'))
  expect(visto.adjuntos[1]).toBeUndefined()
  expect(registros(visto).length).toBe(2)

  // Y el pase a Tienda también se nota en el mensaje siguiente.
  grupo = '[ACME] - Tienda'
  await $.prompt.submit(mensaje('sigo'))
  expect(registros(visto).length).toBe(3)
  expect(visto.adjuntos[2]?.[0]).toContain('«ACME › Tienda»')
})

test('el aviso también va con un mensaje de otra conversación', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  let grupo: string | null = '[ACME] - Tienda'
  const visto = mundo(on, { grupo: () => grupo })

  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))

  grupo = '[ACME] - PORTAL'
  await $.prompt.submit(deOtraConversacion('buscá en la memoria'))
  expect(visto.adjuntos[0]?.[0]).toContain('ahora es del grupo «ACME › Portal»')
})

test('lo que se arma a mitad de la conversación no cuenta como entregado', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  let grupo: string | null = '[ACME] - Tienda'
  const visto = mundo(on, { grupo: () => grupo })

  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  await $.prompt.submit(mensaje('hola'))
  expect(visto.adjuntos[0]).toBeUndefined()

  // La conversación cambia de grupo, el mod se recarga (lo registra con el
  // grupo nuevo) y la app vuelve a armar las instrucciones, que no le llegan
  // a Claude: igual tiene que enterarse con el mensaje siguiente.
  grupo = '[ACME] - PORTAL'
  await $.session.start(INICIO)
  await clock.settle()
  expect(registros(visto).length).toBe(2)
  const rearmadas = await seccion(e => $.prompt.compose(e))
  expect(rearmadas.memoria?.text).toContain('«ACME › Portal»')
  await $.prompt.submit(mensaje('seguimos'))
  expect(visto.adjuntos[1]?.[0]).toContain('ahora es del grupo «ACME › Portal»')
})

test('si el mod se cargó con la conversación empezada, la memoria va con el próximo mensaje', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  const visto = mundo(on, { turnos: 3 })

  await $.session.start(INICIO)
  await clock.settle()
  expect(registros(visto).length).toBe(1)
  // Lo que arme la app ahora no le llega a Claude hasta que se resuma.
  await seccion(e => $.prompt.compose(e))

  await $.prompt.submit(mensaje('seguimos'))
  const [adjunto] = visto.adjuntos[0] ?? []
  expect(adjunto).toContain('# Memoria del grupo')
  expect(adjunto).toContain('Esta conversación es del grupo «ACME › Portal» y su id es `local_1`')
  expect(adjunto).toContain(RESUMEN)

  await $.prompt.submit(mensaje('otra'))
  expect(visto.adjuntos[1]).toBeUndefined()
})

test('si el primer turno corre sin armar las instrucciones, la memoria va con el segundo mensaje', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  const visto = mundo(on)

  await $.session.start(INICIO)
  await clock.settle()
  await $.prompt.submit(mensaje('hola'))
  expect(visto.adjuntos[0]).toBeUndefined()
  await $.turn.complete(finDeTurno)

  await $.prompt.submit(mensaje('seguimos'))
  expect(visto.adjuntos[1]?.[0]).toContain('# Memoria del grupo')
})

test('si un mensaje no entra, el aviso queda para el siguiente', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  let grupo: string | null = '[ACME] - Tienda'
  const visto = mundo(on, { grupo: () => grupo })

  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))

  grupo = '[ACME] - PORTAL'
  await $.prompt.submit(mensaje(RECHAZADO))
  await $.prompt.submit(mensaje('de nuevo'))
  expect(visto.adjuntos[1]?.[0]).toContain('ahora es del grupo «ACME › Portal»')
})

test('después de resumir la conversación le vuelve a pasar la memoria completa', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  const visto = mundo(on)

  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  await $.prompt.submit(mensaje('hola'))
  expect(visto.adjuntos[0]).toBeUndefined()

  await $.session.compact({ trigger: 'manual', messages: [{ role: 'user', text: 'hola', toolUses: [] }] })
  await $.prompt.submit(mensaje('seguimos'))
  expect(visto.adjuntos[1]?.[0]).toContain('# Memoria del grupo')
  expect(visto.adjuntos[1]?.[0]).toContain(RESUMEN)
})

test('el prompt mínimo (bare) queda como está', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-12T15:00:00.000Z') })
  mundo(on)

  await $.session.start({ cwd: '/', surface: 'desktop', isInteractive: true })
  await clock.settle()
  const { sections } = await $.prompt.compose({ ...PEDIDO, traits: ['bare'] })
  expect(sections.map(s => s.id)).toEqual(['intro'])
})

test('si Claude le quita la conexión a la conversación, lo avisa en el chat una vez y se lo cuenta a Claude', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  let conexion: 'bien' | 'quitada' = 'bien'
  const visto = mundo(on, { conexion: () => conexion })

  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  await $.prompt.submit(mensaje('hola'))
  expect(visto.avisos).toEqual([])
  expect(visto.adjuntos[0]).toBeUndefined()

  // La app le manda su lista de conexiones y la de la memoria se va, sin que nada lo diga.
  conexion = 'quitada'
  const pedidos = alServicio(visto).length
  await $.prompt.submit(mensaje('seguimos'))
  expect(visto.avisos.length).toBe(1)
  expect(visto.avisos[0]).toContain('Claude le quitó a esta conversación la conexión con la memoria')
  expect(visto.avisos[0]).toContain('cerrá Claude del todo y volvé a abrirlo')
  // Claude se entera con ese mismo mensaje, y sabe que el usuario ya lo vio.
  const [adjunto] = visto.adjuntos[1] ?? []
  expect(adjunto).toContain('Actualización de la memoria: En esta conversación la memoria no está conectada ahora')
  expect(adjunto).toContain('no la busques con ToolSearch ni digas que guardaste algo')
  expect(adjunto).toContain('El usuario ya lo vio avisado en el chat')
  // Para saberlo no le pidió nada al servicio: se lo preguntó a Claude.
  expect(alServicio(visto).length).toBe(pedidos)

  // Con los mensajes siguientes no repite nada.
  await $.prompt.submit(mensaje('otra cosa'))
  expect(visto.avisos.length).toBe(1)
  expect(visto.adjuntos[2]).toBeUndefined()

  // El comando dice lo mismo, sin sumar otro aviso.
  const { text } = await $.command.run(COMANDO)
  expect(text).toContain('Claude le quitó a esta conversación la conexión con la memoria')
  expect(text).not.toContain('El servicio no respondió')
  expect(visto.avisos.length).toBe(1)
  // Y si la conversación se resume, las instrucciones nuevas lo dicen primero.
  const sinMemoria = await seccion(e => $.prompt.compose(e))
  expect(sinMemoria.memoria?.text.startsWith('# Memoria del grupo\n\nEn esta conversación la memoria no está conectada ahora')).toBe(true)

  // Se instala una versión nueva: el mod se recarga, la conexión vuelve y Claude se entera.
  conexion = 'bien'
  await $.session.start(INICIO)
  await clock.settle()
  await $.prompt.submit(mensaje('sigo'))
  const vuelta = visto.adjuntos.at(-1)?.[0]
  expect(vuelta).toContain('la memoria volvió a estar conectada en esta conversación')
  expect(vuelta).toContain(RESUMEN)
  expect(visto.avisos.length).toBe(1)
  expect((await $.command.run(COMANDO)).text).toContain('El servicio respondió bien.')

  // Hasta que el usuario cierre y abra Claude se puede volver a cortar: avisa de nuevo.
  conexion = 'quitada'
  await $.prompt.submit(mensaje('y esto'))
  expect(visto.avisos.length).toBe(2)
  expect(visto.adjuntos.at(-1)?.[0]).toContain('la memoria no está conectada ahora')
})

test('si la conexión vuelve sin que el mod se recargue, también se lo cuenta a Claude', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  let conexion: 'bien' | 'quitada' = 'bien'
  const visto = mundo(on, { conexion: () => conexion })

  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  conexion = 'quitada'
  await $.prompt.submit(mensaje('hola'))
  expect(visto.avisos.length).toBe(1)

  conexion = 'bien'
  await $.prompt.submit(mensaje('seguimos'))
  const [adjunto] = visto.adjuntos[1] ?? []
  expect(adjunto).toContain('la memoria volvió a estar conectada en esta conversación')
  expect(adjunto).toContain('su id es `local_1`')
  // Y trae el resumen leído de nuevo.
  expect(registros(visto).length).toBe(2)
  expect(adjunto).toContain(RESUMEN)
  await $.prompt.submit(mensaje('otra'))
  expect(visto.adjuntos[2]).toBeUndefined()
  expect(visto.avisos.length).toBe(1)
})

test('una conexión que no anduvo desde que se cargó el mod no es un corte: no avisa en el chat', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  let conexion: 'bien' | 'quitada' = 'quitada'
  const visto = mundo(on, { conexion: () => conexion })

  // La conversación arranca y la conexión todavía no está (se está conectando, o no pudo).
  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  await $.prompt.submit(mensaje('hola'))
  expect(visto.avisos).toEqual([])
  // El comando lo dice en castellano, no con el texto de Claude.
  const { text } = await $.command.run(COMANDO)
  expect(text).toContain('El servicio no respondió: Claude no tiene conectada la memoria en esta conversación')
  expect(text).not.toContain('no connected MCP tool')
  expect(visto.avisos).toEqual([])

  // Cuando aparece, sigue como siempre: el resumen va con el próximo mensaje.
  conexion = 'bien'
  await clock.advance(6_000)
  await $.prompt.submit(mensaje('seguimos'))
  expect(visto.adjuntos.at(-1)?.[0]).toContain(RESUMEN)
  expect(visto.adjuntos.at(-1)?.[0]).not.toContain('volvió a estar conectada')
  expect(visto.avisos).toEqual([])
})

test('avisa aunque el registro haya fallado: alcanza con que Claude la haya dado por conectada', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  let conexion: 'bien' | 'quitada' = 'bien'
  // El mod se recarga un instante antes que la conexión: el primer registro no sale. (En una terminal,
  // donde no hay grupo del costado que revisar antes de cada mensaje.)
  const visto = mundo(on, { conexion: () => conexion, sinConexionAlRegistrar: 1, sinApp: true })

  await $.session.start({ ...INICIO, surface: 'terminal' })
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  expect(registros(visto).length).toBe(0)
  // Con el primer mensaje ve que la conexión está y registra, sin esperar al reintento.
  await $.prompt.submit(mensaje('hola'))
  expect(registros(visto).length).toBe(1)
  expect(visto.adjuntos[0]?.[0]).toContain(RESUMEN)
  expect(visto.avisos).toEqual([])

  // Se corta: es un corte de verdad, porque Claude ya la había dado por conectada.
  conexion = 'quitada'
  await $.prompt.submit(mensaje('seguimos'))
  expect(visto.avisos.length).toBe(1)
  expect(visto.adjuntos.at(-1)?.[0]).toContain('la memoria no está conectada ahora')
})

test('si lo que falla es el servicio y no la conexión, no avisa ni apura el registro', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  const visto = mundo(on, { fallasMemoria: 1, sinApp: true })

  await $.session.start({ ...INICIO, surface: 'terminal' })
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  expect(registros(visto).length).toBe(1)
  // Claude tiene la conexión, pero el servicio no contestó: eso lo arregla el reintento, no el mensaje.
  await $.prompt.submit(mensaje('hola'))
  expect(registros(visto).length).toBe(1)
  expect(visto.avisos).toEqual([])
  expect((await $.command.run(COMANDO)).text).toContain('El servicio respondió bien.')
})

test('el corte de Claude por una conexión que no está se dice en castellano', () => {
  // El texto con que Claude 2.1.321 y 2.1.325 cortan el pedido de un mod a una conexión que no tienen.
  const deClaude = new Error('memoria: $.mcp.call: no connected MCP tool "memoria_contexto" on a server named "memoria"')
  expect(motivoDelPedido(deClaude)).toBe(
    'Claude no tiene conectada la memoria en esta conversación (puede estar conectándose todavía, o no haber podido)',
  )
  // Cualquier otra falla queda como vino.
  expect(motivoDelPedido(new Error('memoria apagada'))).toBe('memoria apagada')
  expect(motivoDelPedido('sin respuesta')).toBe('sin respuesta')
})

test('si el usuario apagó la memoria en la conversación, no avisa nada', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  let conexion: 'bien' | 'apagada' = 'bien'
  const visto = mundo(on, { conexion: () => conexion })

  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  conexion = 'apagada'
  await $.prompt.submit(mensaje('hola'))
  expect(visto.avisos).toEqual([])
  expect(visto.adjuntos[0]).toBeUndefined()
})

test('con un Claude que no sabe decir si la conexión está, no avisa', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  const visto = mundo(on, { claudeViejo: true })

  // La memoria anda, pero ese Claude contesta «no listada» igual: no se le puede creer.
  await $.session.start(INICIO)
  await clock.settle()
  expect(registros(visto).length).toBe(1)
  await seccion(e => $.prompt.compose(e))
  await $.prompt.submit(mensaje('hola'))
  expect(visto.avisos).toEqual([])
  expect(visto.adjuntos[0]).toBeUndefined()
})

test('si no puede dejar el aviso a la vista, le pide a Claude que se lo diga al usuario', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  let conexion: 'bien' | 'quitada' = 'bien'
  const visto = mundo(on, { conexion: () => conexion, sinAvisos: true })

  await $.session.start(INICIO)
  await clock.settle()
  await seccion(e => $.prompt.compose(e))
  conexion = 'quitada'
  await $.prompt.submit(mensaje('hola'))
  expect(visto.avisos).toEqual([])
  const [adjunto] = visto.adjuntos[0] ?? []
  expect(adjunto).toContain('la memoria no está conectada ahora')
  expect(adjunto).toContain('Avisale al usuario, en una línea')
  expect(adjunto).not.toContain('ya lo vio avisado')
})

test('sigue una mejora aceptada y, cuando se instala, avisa en el chat y hace que Claude retome', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-13T15:00:00.000Z') })
  const mejoras: Mejoras = { enCurso: [], avisos: [] }
  const visto = mundo(on, { mejoras })

  await $.session.start(INICIO)
  await clock.settle()
  // Al registrar la conversación pregunta una vez; sin nada en curso no insiste.
  expect(porMejoras(visto).length).toBe(1)
  expect(porMejoras(visto)[0]?.args).toEqual({ sesion: 'local_1', vistos: [] })
  await clock.advance(60_000)
  expect(porMejoras(visto).length).toBe(1)

  // El usuario acepta y Claude llama a memoria_mejorar: empieza a preguntar cada 10 segundos.
  mejoras.enCurso = [4]
  await $.tool.call({ tool: 'mcp__memoria__memoria_mejorar', id: 4, sesion: 'local_1' })
  await clock.advance(10_000)
  await clock.advance(10_000)
  expect(porMejoras(visto).length).toBe(3)
  expect(visto.avisos).toEqual([])

  // El guardián termina y la instala.
  mejoras.enCurso = []
  mejoras.avisos = [
    { id: 4, instalada: true, texto: 'Memoria: mejora 4 instalada. Ahora encuentra sinónimos.', buscado: 'correo del inbox' },
  ]
  await clock.advance(10_000)
  expect(visto.avisos).toEqual(['Memoria: mejora 4 instalada. Ahora encuentra sinónimos.'])
  expect(visto.retomados.length).toBe(1)
  expect(visto.retomados[0]).toContain('La memoria instaló la mejora 4')
  expect(visto.retomados[0]).toContain('Volvé a buscar «correo del inbox»')
  // Le confirma al servicio que lo mostró, y deja de preguntar.
  expect(porMejoras(visto).at(-1)?.args).toEqual({ sesion: 'local_1', vistos: [4] })
  const preguntas = porMejoras(visto).length
  await clock.advance(120_000)
  expect(porMejoras(visto).length).toBe(preguntas)
  expect(visto.avisos.length).toBe(1)
})

test('si la mejora no se pudo, avisa que todo quedó como estaba y no molesta a Claude', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-13T15:00:00.000Z') })
  const mejoras: Mejoras = { enCurso: [5], avisos: [] }
  const visto = mundo(on, { mejoras })

  // Había una en curso al abrir la conversación: la sigue sin que nadie se lo pida.
  await $.session.start(INICIO)
  await clock.settle()
  await clock.advance(10_000)
  expect(porMejoras(visto).length).toBe(2)

  mejoras.enCurso = []
  mejoras.avisos = [
    {
      id: 5,
      instalada: false,
      texto: 'Memoria: no pude hacer la mejora 5; quedó todo como estaba. El cambio rompía dos preguntas de la batería.',
      buscado: 'correo del inbox',
    },
  ]
  await clock.advance(10_000)
  expect(visto.avisos).toEqual([
    'Memoria: no pude hacer la mejora 5; quedó todo como estaba. El cambio rompía dos preguntas de la batería.',
  ])
  expect(visto.retomados).toEqual([])
})

test('un aviso pendiente aparece al volver a abrir la conversación, una sola vez', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-13T15:00:00.000Z') })
  const mejoras: Mejoras = {
    enCurso: [],
    avisos: [{ id: 6, instalada: true, texto: 'Memoria: mejora 6 instalada. Lee mejor las fechas.', buscado: null }],
  }
  const visto = mundo(on, { mejoras, turnos: 8 })

  await $.session.start(INICIO)
  await clock.settle()
  expect(visto.avisos).toEqual(['Memoria: mejora 6 instalada. Lee mejor las fechas.'])
  expect(visto.retomados.length).toBe(1)
  expect(visto.retomados[0]).toContain('Si lo que estabas haciendo dependía de eso, retomalo')

  // Si el mod se recarga, no lo repite.
  await $.session.start(INICIO)
  await clock.settle()
  await clock.advance(30_000)
  expect(visto.avisos.length).toBe(1)
  expect(visto.retomados.length).toBe(1)
})

test('el nombre provisorio y el que informa el servicio', () => {
  expect(grupoDelResumen(RESUMEN)).toBe('ACME › Portal')
  expect(grupoDelResumen('Grupo: Globex · 0 notas vigentes\nTodavía no hay notas en este grupo.')).toBe('Globex')
  expect(grupoDelResumen('otra cosa')).toBe(null)

  expect(provisorio('[ACME] - PORTAL', '/x')).toBe('ACME › PORTAL')
  expect(provisorio(' [Globex] ', '/x')).toBe('Globex')
  expect(provisorio('[Globex] - General', '/x')).toBe('Globex')
  expect(provisorio('Cosas de casa', '/x')).toBe('Otras › Cosas de casa')
  expect(provisorio(null, '/Users/ana/proyectos/recetas')).toBe('Otras › recetas')
  expect(provisorio(null, '/')).toBe('Otras › Sin grupo')
  expect(provisorio(null, '')).toBe('Otras › Sin grupo')
})

// ---------- el agente de mantenimiento ----------

const COPIA = '/Users/ana/.memoria/trabajo/mejora-7'
const CARGO = `${COPIA}-cargo`
const PEDIDO_AGENTE = 'Sos el agente de mantenimiento…\n\n## El pedido\n\nMejora 7 de la memoria.'
// La mejora 7, aceptada en esta conversación y con la copia ya armada por el guardián.
const lista = (): Mejoras => ({
  enCurso: [7],
  avisos: [],
  paraAgente: [{ id: 7, encargo: { copia: COPIA, cargo: CARGO, pedido: PEDIDO_AGENTE } }],
})
const finDeAgente = (agentId: string, answer: string) => ({ ...finDeTurno, answer, agentId })
// El usuario dijo que sí y Claude llama a la herramienta: adentro de esa llamada el mod lanza al agente.
const ACEPTAR = { tool: 'mcp__memoria__memoria_mejorar', id: 7, sesion: 'local_1' } as const
type Motor = { turn: { start: (e: { text: string; turnId: string; agentId: string }) => Promise<unknown> } }
// Como lanza la app desde adentro de un gancho: los ganchos del mod escuchan el arranque del agente.
const conLimites = ($: Motor) => (agentId: string) =>
  $.turn.start({ text: PEDIDO_AGENTE, turnId: `turno-de-${agentId}`, agentId })

test('al aceptar la mejora lanza al agente adentro de esa misma llamada, y avisa cuando termina', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const visto = mundo(on, { mejoras: lista(), alArrancar: conLimites($) })

  await $.session.start(INICIO)
  await clock.settle()
  // Declaró el tipo de agente: solo seis herramientas, y sin preguntarle nunca al usuario.
  expect(visto.tipos.length).toBe(1)
  expect(visto.tipos[0]).toEqual(
    expect.objectContaining({
      name: 'mantenimiento',
      tools: ['Read', 'Glob', 'Grep', 'Edit', 'Write', 'Bash'],
      permissionMode: 'dontAsk',
      effort: 'high',
      omitClaudeMd: true,
    }),
  )
  // Con el reloj solo no lanza nada: un agente lanzado desde ahí quedaría sin límites.
  await clock.advance(30_000)
  expect(visto.lanzados).toEqual([])
  expect(pasos(visto)).toEqual([])

  await $.tool.call(ACEPTAR)
  // Le dio unos segundos al guardián para armar la copia, tomó la mejora y lanzó al agente.
  expect(aLaMemoria(visto).some(l => l.args.esperar_ms === 12_000)).toBe(true)
  expect(pasos(visto)).toEqual([{ id: 7, sesion: 'local_1', paso: 'empezo' }])
  // No le pide una carpeta a la app (no la respeta): el pedido dice dónde está la copia.
  expect(visto.lanzados).toEqual([
    { prompt: PEDIDO_AGENTE, description: 'Mejora 7 de la memoria', subagentType: 'memoria:mantenimiento', cwd: undefined },
  ])
  expect(visto.frenados).toEqual([])

  // Mientras trabaja, cada vuelta avisa que sigue andando, y no lo lanza de nuevo.
  await clock.advance(10_000)
  expect(porMejoras(visto).at(-1)?.args).toEqual({ sesion: 'local_1', vistos: [], agentes: [7] })
  await $.turn.complete(finDeTurno)
  expect(visto.lanzados.length).toBe(1)

  // Su informe final va por la vía de la app: el mod ni lo cambia ni decide su permiso.
  const entrega = await $.tool.call({ tool: 'SubagentHandback', message: 'Bajé el piso.', agentId: 'a1' } as never)
  expect((entrega as { result?: unknown }).result).toBe('hecho')
  expect(visto.consultados).toEqual(['la conversación: mcp__memoria__memoria_mejorar', 'a1: SubagentHandback'])

  // Termina: lo último que dijo va al servicio como informe.
  await $.turn.complete(finDeAgente('a1', ' Bajé el piso de parecido de las notas. '))
  expect(pasos(visto).at(-1)).toEqual({
    id: 7,
    sesion: 'local_1',
    paso: 'termino',
    texto: 'Bajé el piso de parecido de las notas.',
  })
  // Ya no avisa por él, y lo que ese agente quiera hacer después se le niega.
  await clock.advance(10_000)
  expect(porMejoras(visto).at(-1)?.args).toEqual({ sesion: 'local_1', vistos: [] })
  const tarde = await $.tool.call({ tool: 'Read', file_path: 'README.md', agentId: 'a1' })
  expect(tarde.deny).toContain('ya no está en marcha')
})

test('si la copia queda lista más tarde, lo lanza al terminar un turno o con el próximo mensaje', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const mejoras: Mejoras = { enCurso: [7], avisos: [] }
  const visto = mundo(on, { mejoras, alArrancar: conLimites($) })
  await $.session.start(INICIO)
  await clock.settle()
  // Al aceptar, el guardián todavía no terminó: no hay nada que lanzar.
  await $.tool.call(ACEPTAR)
  expect(visto.lanzados).toEqual([])

  // La copia queda lista; el reloj se entera, pero no lanza.
  mejoras.paraAgente = lista().paraAgente
  await clock.advance(10_000)
  expect(visto.lanzados).toEqual([])
  // Termina el turno de la conversación: ahí sí.
  await $.turn.complete(finDeTurno)
  expect(visto.lanzados.length).toBe(1)
  expect(pasos(visto)).toEqual([{ id: 7, sesion: 'local_1', paso: 'empezo' }])

  // Y si el agente se corta, el siguiente sale con el próximo mensaje del usuario.
  if (visto.enLaApp[0]) visto.enLaApp[0].status = 'killed'
  await clock.advance(10_000)
  expect(pasos(visto).at(-1)?.paso).toBe('fallo')
  mejoras.paraAgente = lista().paraAgente
  await clock.advance(10_000)
  expect(visto.lanzados.length).toBe(1)
  await $.prompt.submit(mensaje('seguimos'))
  expect(visto.lanzados.length).toBe(2)
})

test('si el mod no escucha el arranque del agente, lo detiene antes de que haga nada', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  // Sin `alArrancar`: el agente arranca fuera del alcance de los ganchos del mod.
  const visto = mundo(on, { mejoras: lista() })
  await $.session.start(INICIO)
  await clock.settle()
  await $.tool.call(ACEPTAR)

  expect(visto.frenados).toEqual(['a1'])
  expect(pasos(visto)).toEqual([
    { id: 7, sesion: 'local_1', paso: 'empezo' },
    {
      id: 7,
      sesion: 'local_1',
      paso: 'fallo',
      texto: 'No pude ponerle los límites al agente: lo detuve antes de que hiciera nada.',
    },
  ])
  // Y si igual llegara a pedir algo, se le niega.
  const pedido = await $.tool.call({ tool: 'Read', file_path: 'README.md', agentId: 'a1' })
  expect(pedido.deny).toContain('Esta tarea se canceló')
  // El problema se le cuenta al servicio, para que quede en su registro.
  await clock.advance(10_000)
  const contados = porMejoras(visto).flatMap(l => (typeof l.args.problema === 'string' ? [l.args.problema] : []))
  expect(contados).toEqual(['No pude ponerle los límites al agente: lo detuve antes de que hiciera nada.'])
})

test('el agente solo lee la copia, solo edita el servicio y solo corre su cargo, sin que nadie pregunte', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const visto = mundo(on, { mejoras: lista(), alArrancar: conLimites($) })
  await $.session.start(INICIO)
  await clock.settle()
  await $.tool.call(ACEPTAR)
  expect(visto.lanzados.length).toBe(1)
  visto.consultados.length = 0

  // Con cualquier nombre de herramienta, también una que el agente no tiene.
  const pide = (tool: string, args: Record<string, unknown>): Promise<{ result?: unknown; deny?: string }> =>
    $.tool.call({ tool, ...args, agentId: 'a1' } as never)
  const ultima = () => visto.corridas.at(-1)

  // Lee con rutas relativas a su copia; no sale de ella.
  expect((await pide('Read', { file_path: 'crates/servicio/src/buscar.rs' })).result).toBe('hecho')
  expect(ultima()?.file_path).toBe(`${COPIA}/crates/servicio/src/buscar.rs`)
  for (const fuera of ['/etc/passwd', '../mejora-8/Cargo.toml', `${COPIA}/../../clave`, `${COPIA}-cargo`]) {
    expect((await pide('Read', { file_path: fuera })).deny).toContain('dentro de la copia')
  }
  // Busca en la copia entera si no dice dónde, y nunca fuera.
  await pide('Grep', { pattern: 'piso_notas' })
  expect(ultima()?.path).toBe(COPIA)
  await pide('Glob', { pattern: '**/*.rs', path: 'crates/servicio' })
  expect(ultima()?.path).toBe(`${COPIA}/crates/servicio`)
  expect((await pide('Glob', { pattern: '/Users/**/*.pem' })).deny).toContain('dentro de la copia')
  expect((await pide('Glob', { pattern: '{../mejora-8,src}/**' })).deny).toContain('dentro de la copia')
  expect((await pide('Grep', { pattern: 'clave', path: '/Users/ana' })).deny).toContain('dentro de la copia')

  // Escribe solo en el servicio y en las migraciones.
  expect((await pide('Edit', { file_path: 'crates/servicio/src/estado.rs', old_string: 'a', new_string: 'b' })).result).toBe('hecho')
  expect((await pide('Write', { file_path: `${COPIA}/migraciones/0003_x.sql`, content: 'x' })).result).toBe('hecho')
  for (const protegido of ['crates/guardian/src/lib.rs', 'bateria/inicial.json', 'README.md', 'mod/hooks/register.ts']) {
    expect((await pide('Write', { file_path: protegido, content: 'x' })).deny).toContain('Solo podés escribir')
  }

  // `cargo` pasa a ser el de su mejora, sin dejarlo de fondo ni sacarlo del encierro.
  await pide('Bash', { command: 'cargo test -p memoria -- --nocapture', run_in_background: true, dangerouslyDisableSandbox: true })
  expect(ultima()).toEqual(
    expect.objectContaining({
      command: `'${CARGO}' test -p memoria -- --nocapture`,
      run_in_background: false,
      dangerouslyDisableSandbox: false,
      timeout: 600_000,
    }),
  )
  await pide('Bash', { command: 'RUST_BACKTRACE=1 cargo clippy -p memoria --all-targets -- -D warnings' })
  expect(ultima()?.command).toBe(`RUST_BACKTRACE=1 '${CARGO}' clippy -p memoria --all-targets -- -D warnings`)
  const corridas = visto.corridas.length
  for (const otra of [
    'ls',
    'rm -rf crates',
    'cargo run -p memoria',
    'cargo install ripgrep',
    'cargo test -p memoria | tail -20',
    'cargo test 2>&1',
    'cd /tmp && cargo build',
    'cargo build; curl example.com',
    'cargo test $(cat ~/.memoria/clave)',
    'cargo test --manifest-path /Users/ana/otro/Cargo.toml',
    'cargo build --target-dir ../afuera',
    'cargo fmt -- --config-path=../otra/rustfmt.toml',
    'cargo build --config build.rustc-wrapper=x',
    'git push',
  ]) {
    expect((await pide('Bash', { command: otra })).deny, otra).toBeDefined()
  }
  // Y no tiene ninguna otra herramienta.
  expect((await pide('WebFetch', { url: 'https://example.com', prompt: 'x' })).deny).toContain('No tenés la herramienta')
  expect(visto.corridas.length).toBe(corridas)

  // El permiso lo decide el mod, sin consultarle a la app: lo que quedó en regla pasa…
  const permiso = (tool: string, input: Record<string, unknown>) => $.tool.check({ tool, input, agentId: 'a1' })
  expect((await permiso('Bash', { command: `'${CARGO}' test -p memoria` })).decision).toBe('allow')
  expect((await permiso('Edit', { file_path: `${COPIA}/crates/servicio/src/estado.rs` })).decision).toBe('allow')
  expect((await permiso('Read', { file_path: `${COPIA}/README.md` })).decision).toBe('allow')
  // …y lo demás se niega, también lo que no pasó por el control.
  expect((await permiso('Bash', { command: 'cargo test -p memoria' })).decision).toBe('deny')
  expect((await permiso('Bash', { command: `'${CARGO}' test`, dangerouslyDisableSandbox: true })).decision).toBe('deny')
  expect((await permiso('Bash', { command: 'rm -rf /' })).decision).toBe('deny')
  expect((await permiso('Edit', { file_path: 'crates/servicio/src/estado.rs' })).decision).toBe('deny')
  expect((await permiso('Write', { file_path: `${COPIA}/README.md` })).decision).toBe('deny')
  expect((await permiso('WebFetch', { url: 'https://example.com' })).decision).toBe('deny')
  expect(visto.consultados).toEqual([])

  // Lo que hace el usuario en la conversación, y cualquier otro agente, sigue
  // como siempre: el mod no cambia nada ni decide ningún permiso.
  visto.enLaApp.push({ id: 'otro', type: 'general-purpose', status: 'running', description: 'otra cosa' })
  await $.tool.call({ tool: 'Bash', command: 'ls /tmp' })
  expect(ultima()?.command).toBe('ls /tmp')
  await $.tool.call({ tool: 'Read', file_path: '/etc/hosts', agentId: 'otro' })
  expect(ultima()?.file_path).toBe('/etc/hosts')
  expect(visto.consultados).toEqual(['la conversación: Bash', 'otro: Read'])
})

test('si Claude no deja lanzarlo, o el agente se corta, lo anota y lanza el siguiente desde ahí mismo', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const mejoras = { ...lista(), noLanza: 'no se pueden lanzar más agentes' }
  const visto = mundo(on, { mejoras, alArrancar: conLimites($) })
  await $.session.start(INICIO)
  await clock.settle()
  await $.tool.call(ACEPTAR)
  expect(pasos(visto)).toEqual([
    { id: 7, sesion: 'local_1', paso: 'empezo' },
    {
      id: 7,
      sesion: 'local_1',
      paso: 'fallo',
      texto: 'Claude no dejó lanzar el agente: no se pueden lanzar más agentes',
    },
  ])

  // El guardián deja la copia lista otra vez y, al terminar el turno, ahora sí arranca.
  delete (mejoras as Mejoras).noLanza
  mejoras.paraAgente = lista().paraAgente
  await $.turn.complete(finDeTurno)
  expect(visto.lanzados.length).toBe(1)

  // Se corta por un error: lo anota, espera un momento a que el guardián arme
  // la copia de nuevo y lanza el siguiente adentro de ese mismo fin de turno.
  mejoras.paraAgente = lista().paraAgente
  await $.turn.complete({ ...finDeAgente('a1', ''), reason: 'error' })
  expect(pasos(visto).filter(p => p.paso === 'fallo').at(-1)).toEqual({
    id: 7,
    sesion: 'local_1',
    paso: 'fallo',
    texto: 'El agente se cortó por un error de Claude.',
  })
  expect(visto.lanzados.length).toBe(2)
  expect(pasos(visto).at(-1)).toEqual({ id: 7, sesion: 'local_1', paso: 'empezo' })
})

test('si Claude no acepta el tipo de agente, insiste un poco y después lo anota para que se vea en el chat', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const visto = mundo(on, { mejoras: { ...lista(), noRegistra: 'el tipo de agente no es válido' } })
  await $.session.start(INICIO)
  await clock.settle()
  await $.tool.call(ACEPTAR)
  await $.turn.complete(finDeTurno)
  expect(pasos(visto)).toEqual([])
  expect(visto.lanzados).toEqual([])

  await $.turn.complete(finDeTurno)
  expect(pasos(visto).map(p => p.paso)).toEqual(['empezo', 'fallo'])
  const motivo = String(pasos(visto).at(-1)?.texto)
  expect(motivo).toContain('Claude no aceptó el agente de la memoria')
  expect(motivo).toContain('el tipo de agente no es válido')
  expect(visto.lanzados).toEqual([])
  // Lo que salió mal se le cuenta al servicio una vez, para que quede en su registro.
  await clock.advance(20_000)
  const contados = porMejoras(visto).flatMap(l => (typeof l.args.problema === 'string' ? [l.args.problema] : []))
  expect(contados.length).toBe(1)
  expect(contados[0]).toContain('el tipo de agente no es válido')
})

test('si el guardián le saca la mejora, detiene al agente y no avisa nada por él', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const mejoras = lista()
  const visto = mundo(on, { mejoras, alArrancar: conLimites($) })
  await $.session.start(INICIO)
  await clock.settle()
  await $.tool.call(ACEPTAR)
  expect((await $.tool.call({ tool: 'Read', file_path: 'README.md', agentId: 'a1' })).result).toBe('hecho')

  // Pasó demasiado tiempo: el guardián arma la copia de nuevo para otro agente.
  mejoras.conAgente = []
  await clock.advance(10_000)
  expect(visto.frenados).toEqual(['a1'])
  const negada = await $.tool.call({ tool: 'Read', file_path: 'README.md', agentId: 'a1' })
  expect(negada.deny).toContain('Esta tarea se canceló')
  expect((await $.tool.check({ tool: 'Read', input: { file_path: `${COPIA}/README.md` }, agentId: 'a1' })).decision).toBe('deny')
  const anotados = pasos(visto).length
  await $.turn.complete({ ...finDeAgente('a1', ''), reason: 'aborted', isAborted: true })
  expect(pasos(visto).length).toBe(anotados)
})

test('si el servicio no responde justo cuando el agente termina, el informe sale en la vuelta siguiente', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const mejoras = lista()
  const visto = mundo(on, { mejoras, alArrancar: conLimites($) })
  await $.session.start(INICIO)
  await clock.settle()
  await $.tool.call(ACEPTAR)
  mejoras.fallasAlAnotar = 1
  await $.turn.complete(finDeAgente('a1', 'Bajé el piso de parecido.'))
  expect(pasos(visto).filter(p => p.paso === 'termino').length).toBe(1)
  expect(mejoras.conAgente).toEqual([7])

  await clock.advance(10_000)
  expect(pasos(visto).filter(p => p.paso === 'termino').length).toBe(2)
  expect(mejoras.conAgente).toEqual([])
  // Y no insiste más.
  await clock.advance(30_000)
  expect(pasos(visto).filter(p => p.paso === 'termino').length).toBe(2)
})

test('si la app deja de tener al agente, lo anota como caído', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const visto = mundo(on, { mejoras: lista(), alArrancar: conLimites($) })
  await $.session.start(INICIO)
  await clock.settle()
  await $.tool.call(ACEPTAR)
  const [agente] = visto.enLaApp
  if (agente) agente.status = 'killed'
  await clock.advance(10_000)
  expect(pasos(visto).at(-1)).toEqual({
    id: 7,
    sesion: 'local_1',
    paso: 'fallo',
    texto: 'El agente se detuvo antes de terminar.',
  })
})

test('si el mod se recarga con un agente trabajando, lo detiene y lo anota como caído', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-14T15:00:00.000Z') })
  const visto = mundo(on, { mejoras: lista(), alArrancar: conLimites($) })
  await $.session.start(INICIO)
  await clock.settle()
  await $.tool.call(ACEPTAR)
  expect(visto.lanzados.length).toBe(1)

  // Se actualiza el mod: arranca de nuevo, con el agente anotado de antes.
  await $.session.start(INICIO)
  await clock.settle()
  expect(visto.frenados).toEqual(['a1'])
  expect(pasos(visto).at(-1)).toEqual({
    id: 7,
    sesion: 'local_1',
    paso: 'fallo',
    texto: 'La memoria se recargó mientras el agente trabajaba.',
  })
})

test('Claude no ve al agente entre los que puede usar', async ($, on) => {
  const clock = mock.clock(on)
  mundo(on)
  await $.session.start(INICIO)
  await clock.settle()
  const oferta = { description: 'x', source: 'plugin', provider: { plugin: 'memoria', tier: 'user' as const } }
  expect((await $.agent.offer({ agent: 'memoria:mantenimiento', ...oferta })).isOffered).toBe(false)
  expect((await $.agent.offer({ agent: 'Explore', ...oferta })).isOffered).toBe(true)
})

test('las reglas del agente, sin la app', () => {
  const a: AgenteLanzado = { mejora: 7, copia: COPIA, cargo: CARGO }
  expect(normalizar('/a/b/../c/./d//e')).toBe('/a/c/d/e')
  expect(normalizar('/../..')).toBe('/')

  expect(revisar(a, 'Read', { file_path: './docs/../README.md' })).toEqual({ cambio: { file_path: `${COPIA}/README.md` } })
  expect(revisar(a, 'Read', {})).toEqual({ deny: expect.any(String) })
  // La carpeta del servicio sí; algo que solo empieza igual, no.
  expect('cambio' in revisar(a, 'Write', { file_path: 'crates/servicio/Cargo.toml' })).toBe(true)
  expect('deny' in revisar(a, 'Write', { file_path: 'crates/servicio-falso/x.rs' })).toBe(true)
  expect('deny' in revisar(a, 'Write', { file_path: 'migraciones' })).toBe(true)
  expect('deny' in revisar(a, 'Edit', { file_path: `${COPIA}-otra/crates/servicio/x.rs` })).toBe(true)
  // Revisar dos veces da lo mismo.
  const una = revisar(a, 'Bash', { command: 'cargo  fmt   -p memoria' })
  expect(una).toEqual({ cambio: expect.objectContaining({ command: `'${CARGO}' fmt -p memoria` }) })
  if ('cambio' in una) expect(revisar(a, 'Bash', una.cambio)).toEqual(una)
  expect('deny' in revisar(a, 'Bash', { command: 'cargo' })).toBe(true)
  expect('deny' in revisar(a, 'Bash', { command: "cargo test 'x y'" })).toBe(true)
  expect('deny' in revisar(a, 'Bash', { command: 'cargo test\nrm -rf x' })).toBe(true)
  // Una carpeta con comilla en el nombre no rompe la orden.
  const rara = { ...a, cargo: "/Users/ana/mi 'memoria'/mejora-7-cargo" }
  expect(revisar(rara, 'Bash', { command: 'cargo check' })).toEqual({
    cambio: expect.objectContaining({ command: "'/Users/ana/mi '\\''memoria'\\''/mejora-7-cargo' check" }),
  })
  expect(enRegla(a, 'Bash', { command: `'${CARGO}' check`, run_in_background: true })).not.toBe(null)
  expect(enRegla({ ...a, anulado: true }, 'Read', { file_path: `${COPIA}/README.md` })).toContain('se canceló')

  expect(aEncargo({ id: 7, encargo: { copia: `${COPIA}/`, cargo: CARGO, pedido: ' Arreglá esto. ' } })).toEqual({
    id: 7,
    copia: COPIA,
    cargo: CARGO,
    pedido: 'Arreglá esto.',
  })
  expect(aEncargo({ id: 7, encargo: { copia: 'relativa', cargo: CARGO, pedido: 'x' } })).toBe(null)
  expect(aEncargo({ id: 7 })).toBe(null)
})


// ---------- fuentes por una conexión de Claude ----------

const BUSCAR_DE_ANA = { espacio: 'acme', pedir: 'buscar', usuario: 'U0ANA', despues: null, cursor: null, pausa_ms: 1_500 }

test('le lleva a la memoria lo que pide de Slack, por la conexión de Claude y sin pasar por Claude', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  const visto = mundo(on, {
    fuentes: {
      pedidos: [
        { espacio: 'acme', pedir: 'perfil' },
        BUSCAR_DE_ANA,
        { ...BUSCAR_DE_ANA, despues: '1794312000', cursor: 'Q1VSU09S' },
      ],
    },
  })
  await $.session.start(INICIO)
  await clock.settle()
  expect(visto.entregas).toEqual([])
  // Un rato después de registrar la conversación va la primera vuelta.
  await clock.advance(45_000)
  await clock.settle()
  for (let i = 0; i < 4; i += 1) {
    await clock.advance(1_500)
    await clock.settle()
  }
  // Le pidió a la conexión lo que el servicio fue pidiendo, y nada más: el
  // perfil, y los mensajes de ese usuario del más viejo al más nuevo.
  expect(visto.aSlack.map(l => l.tool)).toEqual([PERFIL, BUSCAR, BUSCAR])
  expect(visto.aSlack[0]?.args).toEqual({ response_format: 'detailed' })
  expect(visto.aSlack[1]?.args).toEqual({
    filters: 'from:<@U0ANA>',
    natural_language_query: '',
    sort: 'timestamp',
    sort_dir: 'asc',
    limit: 20,
    include_context: false,
    response_format: 'detailed',
  })
  expect(visto.aSlack[2]?.args).toMatchObject({ filters: 'from:<@U0ANA>', after: '1794312000', cursor: 'Q1VSU09S' })
  // Y al servicio le llevó lo que la conexión contestó, tal cual.
  expect(visto.entregas).toEqual([
    { conexiones: ['slack'] },
    { conexiones: ['slack'], espacio: 'acme', perfil: PERFIL_DE_ANA },
    { conexiones: ['slack'], espacio: 'acme', pagina: PAGINA_DE_ANA },
    { conexiones: ['slack'], espacio: 'acme', pagina: PAGINA_DE_ANA },
  ])
  // Nada de esto pasa por el chat ni por Claude.
  expect(visto.avisos).toEqual([])
  expect(visto.retomados).toEqual([])
  // Cada tanto vuelve a preguntar; si no hay nada, no le pide nada a la conexión.
  await clock.advance(10 * 60_000)
  await clock.settle()
  expect(visto.entregas).toHaveLength(5)
  expect(visto.aSlack).toHaveLength(3)
})

test('sin la conexión de Slack en la conversación, no pregunta por las fuentes', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  const visto = mundo(on)
  await $.session.start(INICIO)
  await clock.settle()
  await clock.advance(11 * 60_000)
  await clock.settle()
  expect(visto.entregas).toEqual([])
  expect(visto.aSlack).toEqual([])
})

test('si la conexión de Slack no contesta, se lo dice a la memoria y no insiste', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  const visto = mundo(on, { fuentes: { pedidos: [BUSCAR_DE_ANA], falla: true } })
  await $.session.start(INICIO)
  await clock.settle()
  await clock.advance(45_000)
  await clock.settle()
  await clock.advance(1_500)
  await clock.settle()
  expect(visto.aSlack).toHaveLength(1)
  expect(visto.entregas).toEqual([
    { conexiones: ['slack'] },
    {
      conexiones: ['slack'],
      espacio: 'acme',
      falla: 'La conexión de Slack de Claude contestó con un error: rate_limited',
    },
  ])
})

test('lo que diga el servicio no le hace pedir a la conexión otra cosa que leer', async ($, on) => {
  const clock = mock.clock(on, { now: Date.parse('2026-11-17T15:00:00.000Z') })
  const visto = mundo(on, {
    fuentes: { pedidos: [{ espacio: 'acme', pedir: 'mandar', usuario: 'U0ANA', texto: 'hola a todos' }, BUSCAR_DE_ANA] },
  })
  await $.session.start(INICIO)
  await clock.settle()
  await clock.advance(45_000)
  await clock.settle()
  await clock.advance(5_000)
  await clock.settle()
  // Un pedido que no conoce corta la vuelta: no le pide nada a la conexión.
  expect(visto.entregas).toEqual([{ conexiones: ['slack'] }])
  expect(visto.aSlack).toEqual([])
})

test('reconoce la conexión de Slack y arma solo pedidos con la forma esperada', () => {
  expect(conexionDeSlack([{ name: 'Read' }, { name: `mcp__abc-123__${BUSCAR}` }])).toBe('abc-123')
  // Un conector local de Slack, con otras herramientas, no es la conexión de Claude.
  expect(conexionDeSlack([{ name: 'mcp__slack-acme__search' }, { name: 'mcp__memoria__memoria_buscar' }])).toBeNull()

  const pedir = (fuente: unknown) => leerPedido(JSON.stringify({ fuente }))
  expect(pedir(null)).toBeNull()
  expect(leerPedido('no es json')).toBeNull()
  expect(leerPedido(JSON.stringify({ en_curso: [], avisos: [] }))).toBeNull()
  expect(pedir({ espacio: 'acme', pedir: 'perfil' })).toEqual({ espacio: 'acme', pedir: 'perfil', pausaMs: 1_500 })
  const buscar = pedir({ ...BUSCAR_DE_ANA, despues: '1794312000', cursor: 'Q1VSU09S', pausa_ms: 999_999 })
  expect(buscar).toEqual({
    espacio: 'acme',
    pedir: 'buscar',
    usuario: 'U0ANA',
    despues: '1794312000',
    cursor: 'Q1VSU09S',
    pausaMs: 10_000,
  })
  // Lo que va derecho al pedido tiene que tener la forma justa: nada de filtros de más.
  for (const malo of [
    { ...BUSCAR_DE_ANA, usuario: 'U0ANA> in:<#C0GENERAL' },
    { ...BUSCAR_DE_ANA, usuario: 'ana' },
    { ...BUSCAR_DE_ANA, despues: 'ayer' },
    { ...BUSCAR_DE_ANA, cursor: 'con espacios' },
    { ...BUSCAR_DE_ANA, espacio: '' },
    { ...BUSCAR_DE_ANA, pedir: 'borrar' },
  ]) {
    expect(pedir(malo)).toBeNull()
  }
  if (buscar?.pedir !== 'buscar') throw new Error('tenía que ser una búsqueda')
  expect(busqueda(buscar)).toMatchObject({ filters: 'from:<@U0ANA>', sort_dir: 'asc', limit: 20, include_context: false })
})
