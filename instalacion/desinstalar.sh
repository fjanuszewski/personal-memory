#!/bin/zsh
# Apaga el servicio memoria y su guardián, y los saca del arranque y de
# Claude (el MCP y el mod). No borra la carpeta de instalación: ahí quedan la
# base (memoria.db), la clave, los respaldos y el modelo de huellas. Para
# sacar también los datos, borrar esa carpeta a mano.
set -uo pipefail

# La carpeta de instalación: la que se diga, la de este mismo archivo si está
# instalado (<carpeta>/bin/), o la de siempre.
if [[ -n "${MEMORIA_DIR:-}" ]]; then DIR="$MEMORIA_DIR"
elif [[ "${0:A:h:t}" == "bin" && -x "${0:A:h}/memoria" ]]; then DIR="${0:A:h:h}"
else DIR="$HOME/.memoria"
fi

claude_bin() {
  if [[ -n "${CLAUDE_BIN:-}" ]]; then print -r -- "$CLAUDE_BIN"; return; fi
  local app
  app="$(ls -d "$HOME/Library/Application Support/Claude/claude-code/"*/*/claude.app/Contents/MacOS/claude 2>/dev/null | sort -V | tail -1)"
  if [[ -n "$app" && -x "$app" ]]; then print -r -- "$app"; return; fi
  if [[ -x "$HOME/.claude/local/claude" ]]; then print -r -- "$HOME/.claude/local/claude"; return; fi
  command -v claude
}
CLAUDE="$(claude_bin)"

for ETIQUETA in personal-memory.guardian personal-memory.memoria; do
  launchctl bootout "gui/$(id -u)/$ETIQUETA" 2>/dev/null && echo "$ETIQUETA apagado."
  rm -f "$HOME/Library/LaunchAgents/$ETIQUETA.plist"
done
echo "Agentes de arranque borrados."
"$CLAUDE" mcp remove memoria --scope user >/dev/null 2>&1 && echo "MCP «memoria» dado de baja."
"$CLAUDE" plugin marketplace remove personal-memory >/dev/null 2>&1 && echo "Mod «memoria» dado de baja."
echo "$DIR queda como estaba: la base, la clave, los respaldos y el modelo."
