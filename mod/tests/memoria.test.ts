import { expect, mock, test } from 'claude-code/testing'
import type { On, PromptComposeInput, PromptComposeResult, PromptSubmitInput } from 'claude-code'

import { grupoDelResumen, provisorio } from '../hooks/grupos'

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
}

type Mejoras = {
  enCurso: number[]
  avisos: { id: number; instalada: boolean; texto: string; buscado: string | null }[]
}

type Visto = {
  /** Lo que se le pidió a la app y a la memoria. */
  llamadas: Llamada[]
  /** Lo que llegó adjunto a cada mensaje. */
  adjuntos: (readonly string[] | undefined)[]
  /** Las líneas que el mod dejó a la vista en el chat. */
  avisos: string[]
  /** Los mensajes que el mod le mandó a Claude para que retome. */
  retomados: string[]
}

// Un mensaje con este texto no entra (como si otro gancho lo rechazara).
const RECHAZADO = 'rechazado'

// La app (sus sesiones), el servicio memoria y el motor, con lo que se les pidió.
function mundo(on: On, o: Mundo = {}): Visto {
  const visto: Visto = { llamadas: [], adjuntos: [], avisos: [], retomados: [] }
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
  on('tool.call', (_, e) =>
    e.tool === 'mcp__memoria__memoria_mejorar' ? { result: 'Mejora aceptada.' } : { deny: 'sin herramienta' },
  )
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

      return texto(JSON.stringify({ en_curso: mejoras.enCurso, avisos: mejoras.avisos }))
    }

    return texto('herramienta desconocida', true)
  })

  return visto
}

const registros = (v: Visto) =>
  v.llamadas.filter(l => l.server === 'memoria' && l.tool === 'memoria_contexto')
const deLaApp = (v: Visto) => v.llamadas.filter(l => l.server === 'ccd_session_mgmt')
const porMejoras = (v: Visto) =>
  v.llamadas.filter(l => l.server === 'memoria' && l.tool === 'memoria_mejora_estado')

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
