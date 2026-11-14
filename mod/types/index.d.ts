/** Lo que la memoria sabe de esta conversación. */
export type Contexto = {
  /** El id con que la memoria conoce la conversación: el de la app (local_…), o el del archivo sin app. */
  sesion: string
  /** El grupo, como se lee: «ACME › Portal», «Globex». */
  grupo: string
  /** El nombre del grupo en el costado de la app, o null si no tiene. */
  grupoApp: string | null
  /** Si el grupo salió de la app (true) o de la carpeta, porque la app no respondió (false). */
  deLaApp: boolean
  /** Si la conversación corre en la app de escritorio, aunque la app todavía no haya respondido. */
  enApp: boolean
  /** El resumen del grupo que dio el servicio al registrarla; null si todavía no llegó. */
  resumen: string | null
  /** El motivo de la última falla, o null si la última vez anduvo. */
  error: string | null
}

/** Qué parte de la memoria ya recibió Claude en esta conversación. */
export type Entrega = {
  /** Si las instrucciones de la conversación ya se armaron: la app las arma una sola vez, al principio. */
  compuesto: boolean
  /** La memoria que ya recibió Claude, en las instrucciones o adjunta a un mensaje; null: ninguna. */
  clave: string | null
}

/** Los avisos de mejoras que ya se mostraron en el chat. */
export type Avisos = {
  /** Ids de las mejoras ya mostradas acá que el servicio todavía no anotó como avisadas. */
  mostrados: number[]
}

/** Un agente de mantenimiento lanzado desde esta conversación. */
export type AgenteLanzado = {
  /** El id de la mejora que trabaja. */
  mejora: number
  /** La carpeta de la copia del código: de ahí no sale. */
  copia: string
  /** El `cargo` de la mejora: lo único que puede correr. */
  cargo: string
  /** El guardián le sacó la mejora: desde acá se le niega todo. */
  anulado?: true
  /** Terminó, y todavía falta avisarle al servicio cómo. */
  aviso?: { paso: 'termino' | 'fallo'; texto: string }
}

/** Los agentes de mantenimiento de esta conversación, por el id que les dio la app. */
export type Agentes = {
  lanzados: Record<string, AgenteLanzado>
}

declare module 'claude-code' {
  interface PluginState {
    memoria: {
      contexto: Shaped<Contexto | null>
      entrega: Shaped<Entrega>
      avisos: Shaped<Avisos>
      agentes: Shaped<Agentes>
    }
  }
}
