-- Estilo del usuario (docs/diseno.md, 4.11): la memoria aprende cómo escribe
-- quien la usa, para que Claude lo siga cuando redacta algo en su nombre.
--
-- Se guarda solo lo que escribió el usuario, nunca lo de los demás. Cada
-- mensaje dice de qué fuente se leyó y en qué lugar lo escribió, porque nadie
-- escribe igual en un chat que en un correo. La primera fuente son las
-- conversaciones con Claude; las que vengan (mensajería, correo) usan las
-- mismas tablas.

create table mensajes_propios (
  id            integer primary key autoincrement,
  fuente        text not null,              -- de dónde se leyó: 'claude'
  lugar         text not null,              -- dónde lo escribió: 'claude', 'whatsapp', 'correo'…
  conversacion  text,                       -- la conversación, como la nombra la fuente
  para          text,                       -- a quién le escribía, si la fuente lo sabe
  idioma        text not null default '',   -- 'es', 'en'…; '' si es muy corto para saberlo
  fecha         text not null,
  contenido     text not null,              -- con los secretos tapados
  marca         text not null,              -- huella corta del texto
  -- El mismo mensaje puede estar en dos archivos (una conversación abierta
  -- como copia de otra): se guarda una vez.
  unique (fuente, marca, fecha)
);
create index mensajes_propios_por_lugar on mensajes_propios (lugar, idioma, fecha desc);

-- Hasta dónde se leyó cada cosa de cada fuente (para las conversaciones con
-- Claude, cada archivo).
create table lecturas_propias (
  fuente  text not null,
  origen  text not null,
  hasta   integer not null default 0,   -- desde dónde se sigue la próxima vez
  tamano  integer not null default 0,   -- cuánto pesaba la última vez que se leyó
  primary key (fuente, origen)
);
