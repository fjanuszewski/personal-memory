import { expect, mock, test } from 'claude-code/testing'
import type { On, PromptComposeInput, PromptComposeResult, PromptSubmitInput } from 'claude-code'

import { aEncargo, enRegla, normalizar, revisar } from '../hooks/agente'
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
  /** Las mejoras de la conversación, como las lleva el servicio. La prueba las va cambiando. */
  mejoras?: Mejoras
  /**
   * Lo que hace la app al lanzar un agente desde adentro de un gancho: avisar
   * su arranque a los ganchos del mod. Sin esto, el agente arranca sin que el
   * mod lo escuche, como cuando se lo lanza desde un reloj.
   */
  alArrancar?: (agentId: string) => Promise<unknown>
}

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
  }
  let fallas = o.fallasMemoria ?? 0
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
  on('mcp.call', (_, e) => {
    const llamada = e as unknown as Llamada
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

