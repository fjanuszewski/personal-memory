#!/bin/zsh
# Instala (o actualiza) la memoria en esta Mac. Lo único que hace falta tener
# antes es Rust (para compilar) y Claude Code.
#
# Todo lo que queda instalado va a una carpeta aparte (~/.memoria, o la que
# diga MEMORIA_DIR). Después de instalar, nada depende de la carpeta de este
# repositorio: se puede seguir trabajando en ella, moverla o borrarla, y lo
# instalado sigue andando igual. Ahí quedan también los datos de cada uno, que
# nunca pasan por el repositorio.
#
#   <carpeta>/bin/         los programas (servicio, guardián, y los comandos
#                          para sacar la llave del agente y para desinstalar)
#   <carpeta>/catalogo/    el mod, de donde lo lee Claude
#   <carpeta>/codigo/      una copia propia del código de la versión instalada,
#                          donde el guardián hace y guarda las mejoras
#   <carpeta>/memoria.db   la base; y al lado la clave, los ajustes, los
#                          respaldos y el modelo de huellas
#
# Qué hace, en orden:
#   1. compila el servicio y el guardián y los copia (guarda el servicio anterior);
#   2. anota en `ajustes` lo que se haya elegido al llamarlo;
#   3. deja lista la base y baja el modelo de huellas si falta (192 MB, una vez);
#   4. si esta Mac usó la versión anterior, trae lo que había en Postgres;
#   5. deja en `codigo` la versión del código que se instaló;
#   6. instala el agente de arranque del servicio y espera a que responda;
#   7. da de alta el MCP «memoria» para todas las conversaciones (nivel usuario);
#   8. copia el mod a `catalogo` y lo da de alta en Claude;
#   9. instala el agente de arranque del guardián.
# Se puede correr las veces que haga falta. El guardián solo se actualiza
# así, a mano: el agente de mejoras nunca lo toca.
#
# Para instalar en otra carpeta:
#   MEMORIA_DIR=/otra/carpeta instalacion/instalar.sh
#
# Para que lo conversado quede guardado con tu nombre en vez de «Usuario»:
#   MEMORIA_NOMBRE=Ana instalacion/instalar.sh
#
# Para elegir cómo se calculan las huellas (se recuerda para la próxima vez):
#   MEMORIA_MODELO_HUELLAS=e5-small instalacion/instalar.sh   # otro modelo interno
#   MEMORIA_HUELLAS=ollama instalacion/instalar.sh            # con Ollama (bge-m3)
set -euo pipefail

REPO="${0:A:h:h}"
CASA="$HOME"
DIR="${${MEMORIA_DIR:-$CASA/.memoria}:A}"
CODIGO="$DIR/codigo"
CATALOGO="$DIR/catalogo"
ETIQUETA="personal-memory.memoria"
GUARDIAN="personal-memory.guardian"
CATALOGO_NOMBRE="personal-memory"
MOD="memoria@$CATALOGO_NOMBRE"
AGENTES="$CASA/Library/LaunchAgents"
DIRECCION="http://127.0.0.1:7337"
DOMINIO="gui/$(id -u)"
export PATH="/opt/homebrew/opt/rustup/bin:$CASA/.cargo/bin:$PATH"

paso() { print -P "%F{cyan}›%f $1"; }

# El Claude Code más nuevo que haya: el de la app suele estar más al día que
# el de la terminal, y el mod necesita 2.1.321 o más.
claude_bin() {
  if [[ -n "${CLAUDE_BIN:-}" ]]; then print -r -- "$CLAUDE_BIN"; return; fi
  local app
  app="$(ls -d "$CASA/Library/Application Support/Claude/claude-code/"*/*/claude.app/Contents/MacOS/claude 2>/dev/null | sort -V | tail -1)"
  if [[ -n "$app" && -x "$app" ]]; then print -r -- "$app"; return; fi
  if [[ -x "$CASA/.claude/local/claude" ]]; then print -r -- "$CASA/.claude/local/claude"; return; fi
  command -v claude
}

# Copia el archivo de un agente de launchd con las carpetas de esta
# instalación y lo (re)arranca.
arrancar() {
  local etiqueta="$1" archivo="$AGENTES/$1.plist"
  mkdir -p "$AGENTES"
  sed -e "s#__CASA__#$CASA#g" -e "s#__DIR__#$DIR#g" \
    "$REPO/instalacion/$etiqueta.plist" > "$archivo"
  plutil -lint -s "$archivo"
  if launchctl print "$DOMINIO/$etiqueta" >/dev/null 2>&1; then
    launchctl bootout "$DOMINIO/$etiqueta" || true
    sleep 1
  fi
  launchctl bootstrap "$DOMINIO" "$archivo"
  launchctl kickstart -k "$DOMINIO/$etiqueta"
}

# Reemplaza un archivo de <carpeta>/bin de una sola vez (nunca queda a medio copiar).
copiar() {
  cp "$1" "$DIR/bin/${1:t}.nuevo"
  mv -f "$DIR/bin/${1:t}.nuevo" "$DIR/bin/${1:t}"
}

# De qué versión del código sale lo que se instala. Con cambios sin guardar en
# git no hay una versión exacta, y las mejoras automáticas quedan en pausa.
VERSION=""
if [[ -z "$(git -C "$REPO" status --porcelain 2>/dev/null)" ]]; then
  VERSION="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)"
fi

# Antes de tocar nada: si el guardián instaló mejoras que este código no
# trae, instalar encima las sacaría del programa.
if [[ -n "$VERSION" && -d "$CODIGO/.git" ]]; then
  git -C "$CODIGO" fetch --quiet "$REPO" HEAD
  ANTERIOR="$(cat "$DIR/instalada" 2>/dev/null || true)"
  if [[ -n "$ANTERIOR" && -z "${MEMORIA_PISAR_MEJORAS:-}" ]] \
     && git -C "$CODIGO" cat-file -e "$ANTERIOR^{commit}" 2>/dev/null \
     && ! git -C "$CODIGO" merge-base --is-ancestor "$ANTERIOR" "$VERSION"; then
    echo "Lo instalado tiene mejoras automáticas que este código no trae." >&2
    echo "Están en $CODIGO (versión ${ANTERIOR[1,7]}). Para sumarlas a este repositorio:" >&2
    echo "  git -C \"$REPO\" pull \"$CODIGO\" instalada" >&2
    echo "Para instalar igual y dejarlas afuera del programa (quedan guardadas en esa carpeta):" >&2
    echo "  MEMORIA_PISAR_MEJORAS=1 $0" >&2
    exit 1
  fi
fi

paso "Compilando el servicio y el guardián (la primera vez tarda unos minutos)"
(cd "$REPO" && cargo build --release --locked -p memoria -p guardian)

# Mientras se instala a mano no tiene que haber nadie usando la base: ni el
# guardián instalando una mejora ni el servicio anterior escribiendo.
for ETIQUETA_PRENDIDA in "$GUARDIAN" "$ETIQUETA"; do
  if launchctl print "$DOMINIO/$ETIQUETA_PRENDIDA" >/dev/null 2>&1; then
    launchctl bootout "$DOMINIO/$ETIQUETA_PRENDIDA" || true
  fi
done

paso "Copiando los programas a $DIR/bin"
mkdir -p "$DIR/bin" "$CASA/Library/Logs/memoria"
chmod 700 "$DIR"
if [[ -f "$DIR/bin/memoria" ]]; then
  cp -p "$DIR/bin/memoria" "$DIR/bin/memoria.anterior"
fi
copiar "$REPO/target/release/memoria"
copiar "$REPO/target/release/memoria-guardian"
# Lo que hace falta después de instalar queda acá, para no depender del repositorio.
copiar "$REPO/instalacion/conectar-agente.sh"
copiar "$REPO/instalacion/desinstalar.sh"

paso "Ajustes"
# Lo que se haya pedido al llamar al instalador queda anotado, y vale también
# para las próximas veces. Sin nada, va todo por defecto.
AJUSTES="$DIR/ajustes"
touch "$AJUSTES"
chmod 600 "$AJUSTES"
for NOMBRE in MEMORIA_NOMBRE MEMORIA_HUELLAS MEMORIA_MODELO_HUELLAS MEMORIA_OLLAMA MEMORIA_HILOS MEMORIA_UMBRALES; do
  VALOR="${(P)NOMBRE:-}"
  if [[ -n "$VALOR" ]]; then
    grep -v "^$NOMBRE=" "$AJUSTES" > "$AJUSTES.nuevo" || true
    print -r -- "$NOMBRE=$VALOR" >> "$AJUSTES.nuevo"
    mv -f "$AJUSTES.nuevo" "$AJUSTES"
  fi
done
if [[ -s "$AJUSTES" ]]; then cat "$AJUSTES"; else echo "Sin cambios: va todo por defecto."; fi

paso "Base y modelo de huellas (la primera vez baja el modelo: 192 MB)"
# Sin el modelo la memoria anda igual, pero busca solo por palabras y no
# reconoce las notas repetidas que usan otras palabras.
if ! MEMORIA_DIR="$DIR" "$DIR/bin/memoria" preparar; then
  echo "No pude dejar listo el modelo de huellas: la memoria va a buscar solo por palabras hasta que se pueda." >&2
  echo "Se puede probar de nuevo con: MEMORIA_DIR=\"$DIR\" $DIR/bin/memoria preparar" >&2
fi

paso "Datos de la versión anterior (Postgres), si los hay"
/usr/bin/python3 -I "$REPO/instalacion/importar-de-postgres.py" "$DIR/memoria.db" || \
  echo "No pude traer los datos de Postgres: la memoria arranca con lo que tenga. Se puede probar de nuevo a mano con el servicio apagado." >&2

paso "Código de la versión instalada (en $CODIGO)"
# El guardián arma cada mejora a partir del código del que salió lo instalado,
# y guarda ahí lo que hace: nunca en el repositorio de trabajo.
if [[ -n "$VERSION" ]]; then
  if [[ ! -d "$CODIGO/.git" ]]; then
    git clone --quiet --no-hardlinks "$REPO" "$CODIGO"
    # Es una copia propia: no queda atada a la carpeta de donde salió, y lo
    # que guarde el guardián va a su nombre.
    git -C "$CODIGO" remote remove origin
    git -C "$CODIGO" config user.name "memoria-guardian"
    git -C "$CODIGO" config user.email "memoria-guardian@localhost"
  fi
  git -C "$CODIGO" checkout --quiet --force -B instalada "$VERSION"
  print -r -- "$VERSION" > "$DIR/instalada"
  echo "Commit ${VERSION[1,7]}."
else
  print -r -- "sin-guardar" > "$DIR/instalada"
  echo "Hay cambios sin guardar en git: las mejoras automáticas quedan en pausa hasta guardarlos y correr de nuevo este instalador." >&2
fi

paso "Agente de arranque del servicio"
arrancar "$ETIQUETA"

paso "Esperando a que el servicio responda"
for _ in {1..60}; do
  if curl -fsS "$DIRECCION/salud" >/dev/null 2>&1; then break; fi
  sleep 1
done
if ! curl -fsS "$DIRECCION/salud"; then
  echo "\nEl servicio no respondió. Mirá $CASA/Library/Logs/memoria/" >&2
  exit 1
fi
echo

paso "Alta del MCP «memoria» para todas las conversaciones"
CLAVE="$(cat "$DIR/clave")"
CLAUDE="$(claude_bin)"
# Si ya está dado de alta con la misma dirección y la misma clave no se toca:
# darlo de baja y de alta de nuevo puede cortar a las conversaciones abiertas.
ALTA="$("$CLAUDE" mcp get memoria 2>/dev/null || true)"
if [[ "$ALTA" == *"$DIRECCION/mcp"* && "$ALTA" == *"$CLAVE"* ]]; then
  echo "Ya estaba dado de alta."
else
  "$CLAUDE" mcp remove memoria --scope user >/dev/null 2>&1 || true
  "$CLAUDE" mcp add --transport http --scope user memoria "$DIRECCION/mcp" \
    --header "Authorization: Bearer $CLAVE" >/dev/null
  "$CLAUDE" mcp get memoria | sed -e "s/$CLAVE/…/g"
fi

paso "Mod «memoria» (en $CATALOGO)"
# Claude lee el mod de la carpeta de instalación, no del repositorio.
mkdir -p "$CATALOGO/mod"
rsync -a --delete "$REPO/.claude-plugin/" "$CATALOGO/.claude-plugin/"
rsync -a --delete --exclude '/tests/' --exclude '/.claude-plugin/types/' "$REPO/mod/" "$CATALOGO/mod/"
# Dónde tiene anotado Claude ese catálogo, si lo tiene.
ANOTADO="$("$CLAUDE" plugin marketplace list --json 2>/dev/null | /usr/bin/python3 -I -c '
import json, sys
try:
    catalogos = json.load(sys.stdin)
except ValueError:
    catalogos = []
print(next((c.get("path", "") for c in catalogos if c.get("name") == sys.argv[1]), ""))
' "$CATALOGO_NOMBRE" || true)"
if [[ "$ANOTADO" != "$CATALOGO" ]]; then
  if [[ -n "$ANOTADO" ]]; then
    "$CLAUDE" plugin marketplace remove "$CATALOGO_NOMBRE" >/dev/null 2>&1 || true
  fi
  "$CLAUDE" plugin marketplace add "$CATALOGO"
fi
if "$CLAUDE" plugin list 2>/dev/null | grep -q "$MOD"; then
  "$CLAUDE" plugin update "$MOD" --scope user || true
else
  "$CLAUDE" plugin install "$MOD" --scope user
fi

paso "Agente de arranque del guardián"
arrancar "$GUARDIAN"

print -P "%F{green}✔%f Memoria instalada en $DIR. Las conversaciones nuevas ya la tienen."
if [[ ! -f "$DIR/agente.token" ]]; then
  echo "Para las mejoras automáticas falta una cosa, una sola vez: darle una llave al agente de mejoras."
  echo "  Correr $DIR/bin/conectar-agente.sh en la app Terminal."
fi
