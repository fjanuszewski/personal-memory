#!/bin/zsh
# Le da al agente de mejoras una llave propia para usar Claude sin la app.
# Hace falta una vez (y de nuevo si la llave vence, al año): sin esto el
# guardián no puede hacer trabajar al agente y las mejoras quedan como «no se
# pudo». Hay que correrlo desde la app Terminal, porque abre el navegador
# para que el usuario autorice con su cuenta.
set -euo pipefail

CASA="$HOME"
# La carpeta de instalación: la que se diga, la de este mismo archivo si está
# instalado (<carpeta>/bin/), o la de siempre.
if [[ -n "${MEMORIA_DIR:-}" ]]; then DIR="$MEMORIA_DIR"
elif [[ "${0:A:h:t}" == "bin" && -x "${0:A:h}/memoria" ]]; then DIR="${0:A:h:h}"
else DIR="$CASA/.memoria"
fi
LLAVE_EN="$DIR/agente.token"

# El Claude Code más nuevo que haya (el mismo criterio que instalar.sh).
claude_bin() {
  if [[ -n "${CLAUDE_BIN:-}" ]]; then print -r -- "$CLAUDE_BIN"; return; fi
  local app
  app="$(ls -d "$CASA/Library/Application Support/Claude/claude-code/"*/*/claude.app/Contents/MacOS/claude 2>/dev/null | sort -V | tail -1)"
  if [[ -n "$app" && -x "$app" ]]; then print -r -- "$app"; return; fi
  if [[ -x "$CASA/.claude/local/claude" ]]; then print -r -- "$CASA/.claude/local/claude"; return; fi
  command -v claude
}
CLAUDE="$(claude_bin)"

# Prueba una llave con un pedido mínimo, en un entorno limpio como el del guardián.
anda() {
  env -i HOME="$CASA" USER="$(id -un)" PATH="/usr/bin:/bin" CLAUDE_CODE_OAUTH_TOKEN="$1" \
    "$CLAUDE" -p "Respondé solo con la palabra: listo" --max-turns 1 --output-format json \
    --strict-mcp-config --mcp-config '{"mcpServers":{}}' --no-session-persistence 2>/dev/null \
    | grep -q '"is_error":false'
}

if [[ -f "$LLAVE_EN" ]] && anda "$(cat "$LLAVE_EN")"; then
  print -P "%F{green}✔%f El agente ya tiene una llave que anda. No hace falta hacer nada."
  exit 0
fi

print "Paso 1: se va a abrir el navegador para que autorices con tu cuenta de Claude."
print "Paso 2: al terminar, la Terminal te muestra una llave larga que empieza con «sk-ant-oat»."
print "        Copiala entera."
print
"$CLAUDE" setup-token
print
print -n "Paso 3: pegá acá la llave (no se va a ver mientras la pegás) y apretá Enter: "
read -rs LLAVE
print
# Si al copiarla quedó partida en renglones, se junta.
LLAVE="${LLAVE//[[:space:]]/}"
if [[ "$LLAVE" != sk-ant-oat* ]]; then
  print "Eso no parece la llave: tiene que empezar con «sk-ant-oat». No guardé nada." >&2
  exit 1
fi

print "Probando la llave…"
if ! anda "$LLAVE"; then
  print "La llave no anduvo (puede haberse copiado incompleta). No guardé nada: corré esto de nuevo." >&2
  exit 1
fi

mkdir -p "$DIR"
chmod 700 "$DIR"
( umask 077; print -r -- "$LLAVE" > "$LLAVE_EN.nuevo" )
mv -f "$LLAVE_EN.nuevo" "$LLAVE_EN"
print -P "%F{green}✔%f Listo: el agente de mejoras ya puede trabajar solo. La llave quedó en $LLAVE_EN, legible solo por vos."
