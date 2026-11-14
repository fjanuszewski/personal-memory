-- El agente de mantenimiento corre dentro de una conversación de Claude, de
-- fondo, lanzado por el mod: ya no hace falta ninguna llave. El guardián
-- prepara la copia del código y deja el encargo; el mod de la conversación lo
-- toma, lanza al agente y avisa cuando termina; el guardián revisa, prueba,
-- mide e instala como siempre.
--
-- Mientras una mejora está `en_curso`, `fase` dice en qué parte del recorrido:
--   preparando        el guardián arma la copia del código
--   esperando_agente  lista para que una conversación de Claude la tome
--   con_agente        un agente la está trabajando en una conversación
--   agente_listo      el agente terminó: espera al guardián
--   agente_fallo      el agente no pudo terminar: el guardián decide si se lanza otro
--   probando          el guardián revisa, compila, prueba, mide e instala

alter table mejoras add column fase text
  check (fase is null or fase in ('preparando', 'esperando_agente', 'con_agente', 'agente_listo', 'agente_fallo',
                                    'probando'));
alter table mejoras add column fase_desde text;      -- cuándo empezó la fase
-- Lo que el guardián le deja al mod, como texto JSON: la carpeta de la copia,
-- el `cargo` de esta mejora y el pedido para el agente. El servicio lo pasa tal cual.
alter table mejoras add column encargo text;
alter table mejoras add column informe text;         -- lo último que dijo el agente al terminar
alter table mejoras add column desde_version text;   -- el commit del que partió la copia
alter table mejoras add column agente_sesion text;   -- la conversación donde corre el agente
alter table mejoras add column agente_visto text;    -- la última vez que el mod avisó que sigue andando
alter table mejoras add column lanzamientos integer not null default 0; -- agentes lanzados en este intento
