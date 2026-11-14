#!/usr/bin/python3 -I
"""Trae a la base nueva (un archivo de SQLite) lo que la memoria tenía en Postgres.

Solo hace falta una vez, y solo si esta máquina usó la versión anterior de la
memoria, la que guardaba todo en Postgres. Lee de Postgres sin cambiarle nada
y escribe en el archivo nuevo únicamente si está vacío.

Uso: importar-de-postgres.py [archivo.db] [base de Postgres]
  archivo.db        por defecto ~/.memoria/memoria.db (lo crea «memoria preparar»)
  base de Postgres  por defecto «memoria»

El servicio tiene que estar apagado. Las huellas y el índice de palabras no
se traen: el servicio los arma solo la próxima vez que arranca.
"""
import json
import os
import sqlite3
import subprocess
import sys

PSQL_POSIBLES = [
    os.environ.get("MEMORIA_PG_BIN", "") + "/psql",
    "/Applications/Postgres.app/Contents/Versions/18/bin/psql",
    "/Applications/Postgres.app/Contents/Versions/latest/bin/psql",
    "/opt/homebrew/bin/psql",
    "/usr/local/bin/psql",
    "/usr/bin/psql",
]


def psql():
    for candidato in PSQL_POSIBLES:
        if os.path.isfile(candidato) and os.access(candidato, os.X_OK):
            return candidato
    return None


def marca(texto):
    """La misma marca que calcula el servicio (`texto::marca`)."""
    datos = texto.encode("utf-8")

    def pasada(h):
        for byte in datos:
            h = ((h ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
        return h

    return "%016x%016x" % (pasada(0xCBF29CE484222325), pasada(0x6C62272E07BB0142))


def fecha(columna):
    """Una fecha de Postgres escrita como las guarda el servicio (texto, en UTC)."""
    return "to_char(%s at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US') || '+00:00'" % columna


def leer(programa, base, consulta):
    """Las filas de una consulta, cada una como un diccionario."""
    orden = [programa, "-X", "-h", "localhost", "-d", base, "-At", "-v", "ON_ERROR_STOP=1", "-c",
             "select row_to_json(t) from (%s) t" % consulta]
    salida = subprocess.run(orden, capture_output=True, text=True, check=False)
    if salida.returncode != 0:
        raise RuntimeError(salida.stderr.strip() or "psql falló")
    return [json.loads(renglon) for renglon in salida.stdout.splitlines() if renglon.strip()]


def lista(valor):
    return json.dumps(valor or [], ensure_ascii=False)


def columnas_de(programa, base, tabla):
    filas = leer(programa, base,
                 "select column_name as nombre from information_schema.columns "
                 "where table_schema = 'public' and table_name = '%s'" % tabla)
    return {f["nombre"] for f in filas}


def main():
    archivo = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/.memoria/memoria.db")
    base = sys.argv[2] if len(sys.argv) > 2 else "memoria"
    programa = psql()
    if programa is None:
        print("No encuentro psql: no hay nada que traer de Postgres.")
        return 0
    try:
        tablas = {f["tabla"] for f in leer(programa, base,
                                             "select table_name as tabla from information_schema.tables "
                                             "where table_schema = 'public'")}
    except (RuntimeError, OSError) as falla:
        print("No pude leer la base «%s» de Postgres (%s): no traigo nada." % (base, str(falla).splitlines()[0]))
        return 0
    if "notas" not in tablas or "grupos" not in tablas:
        print("En Postgres no hay una memoria anterior: no hay nada que traer.")
        return 0
    if not os.path.isfile(archivo):
        print("No existe %s: primero corré «memoria preparar»." % archivo, file=sys.stderr)
        return 1

    nueva = sqlite3.connect(archivo, timeout=30)
    try:
        ya_hay = sum(nueva.execute("select count(*) from %s" % t).fetchone()[0]
                     for t in ("grupos", "conversaciones", "notas", "fragmentos"))
        if ya_hay:
            print("La base nueva ya tiene datos: no traigo nada, para no mezclar.")
            return 0

        grupos = leer(programa, base, "select id, empresa, proyecto, carpetas, grupos_app from grupos order by id")
        conversaciones = leer(programa, base,
                              "select id, transcripcion, titulo, carpeta, grupo_id, leida_hasta, %s as actualizada "
                              "from conversaciones order by actualizada" % fecha("actualizada"))
        notas = leer(programa, base,
                     "select id, grupo_id, tipo::text as tipo, titulo, cuerpo, estado::text as estado, "
                     "reemplazada_por, origenes, autor, repos, etiquetas, %s as creada, %s as actualizada "
                     "from notas order by id" % (fecha("creada"), fecha("actualizada")))
        fragmentos = []
        if "fragmentos" in tablas:
            fragmentos = leer(programa, base,
                              "select id, conversacion, grupo_id, desde, hasta, contenido, %s as fecha "
                              "from fragmentos order by id" % fecha("fecha"))
        mejoras, senales, preguntas = [], [], []
        if "mejoras" in tablas:
            con = columnas_de(programa, base, "mejoras")
            opcional = lambda nombre, si_falta: nombre if nombre in con else "%s as %s" % (si_falta, nombre)
            mejoras = leer(programa, base,
                           "select id, resumen, detalle, firma, origen, conversacion, %s, %s, toca_base, borra_datos, "
                           "suma_librerias, estado::text as estado, motivo, cambios, version_antes, version_despues, "
                           "puntaje_antes, puntaje_despues, %s, %s, avisada, %s as creada, %s, %s as terminada "
                           "from mejoras order by id"
                           % (opcional("grupo_id", "null::bigint"), opcional("ejemplo", "'{}'::jsonb"),
                              opcional("intentos", "0"), opcional("rama", "null::text"), fecha("creada"),
                              (fecha("empezada") + " as empezada") if "empezada" in con else "null::text as empezada",
                              fecha("terminada")))
        if "senales" in tablas:
            senales = leer(programa, base,
                           "select id, tipo, conversacion, detalle, %s as fecha from senales order by id" % fecha("fecha"))
        if "bateria" in tablas:
            preguntas = leer(programa, base,
                             "select id, pregunta, grupo_id, esperadas, desde_mejora, %s as creada "
                             "from bateria order by id" % fecha("creada"))

        with nueva:  # todo o nada
            nueva.executemany(
                "insert into grupos (id, empresa, proyecto, carpetas, grupos_app) values (?, ?, ?, ?, ?)",
                [(g["id"], g["empresa"], g["proyecto"] or "", lista(g["carpetas"]), lista(g["grupos_app"]))
                 for g in grupos])
            nueva.executemany(
                "insert into conversaciones (id, transcripcion, titulo, carpeta, grupo_id, leida_hasta, actualizada) "
                "values (?, ?, ?, ?, ?, ?, ?)",
                [(c["id"], c["transcripcion"], c["titulo"], c["carpeta"], c["grupo_id"], c["leida_hasta"],
                  c["actualizada"]) for c in conversaciones])
            nueva.executemany(
                "insert into notas (id, grupo_id, tipo, titulo, cuerpo, estado, reemplazada_por, origenes, autor, "
                "repos, etiquetas, creada, actualizada) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                [(n["id"], n["grupo_id"], n["tipo"], n["titulo"], n["cuerpo"], n["estado"], n["reemplazada_por"],
                  lista(n["origenes"]), n["autor"], lista(n["repos"]), lista(n["etiquetas"]), n["creada"],
                  n["actualizada"]) for n in notas])
            # Un mismo texto no va dos veces en el mismo grupo: se queda el primero.
            vistos, repetidos, filas = set(), 0, []
            for f in fragmentos:
                huella = marca(f["contenido"])
                if (f["grupo_id"], huella) in vistos:
                    repetidos += 1
                    continue
                vistos.add((f["grupo_id"], huella))
                filas.append((f["id"], f["conversacion"], f["grupo_id"], f["desde"], f["hasta"], f["contenido"],
                              f["fecha"], huella))
            nueva.executemany(
                "insert into fragmentos (id, conversacion, grupo_id, desde, hasta, contenido, fecha, marca) "
                "values (?, ?, ?, ?, ?, ?, ?, ?)", filas)
            nueva.executemany(
                "insert into mejoras (id, resumen, detalle, firma, origen, conversacion, grupo_id, ejemplo, toca_base, "
                "borra_datos, suma_librerias, estado, motivo, cambios, version_antes, version_despues, puntaje_antes, "
                "puntaje_despues, intentos, rama, avisada, creada, empezada, terminada) "
                "values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                [(m["id"], m["resumen"], m["detalle"], m["firma"], m["origen"], m["conversacion"], m["grupo_id"],
                  json.dumps(m["ejemplo"] or {}, ensure_ascii=False), int(bool(m["toca_base"])),
                  int(bool(m["borra_datos"])), int(bool(m["suma_librerias"])), m["estado"], m["motivo"], m["cambios"],
                  m["version_antes"], m["version_despues"], m["puntaje_antes"], m["puntaje_despues"],
                  m["intentos"] or 0, m["rama"], int(bool(m["avisada"])), m["creada"], m["empezada"], m["terminada"])
                 for m in mejoras])
            nueva.executemany(
                "insert into senales (id, tipo, conversacion, detalle, fecha) values (?, ?, ?, ?, ?)",
                [(s["id"], s["tipo"], s["conversacion"], json.dumps(s["detalle"], ensure_ascii=False), s["fecha"])
                 for s in senales])
            nueva.executemany(
                "insert into bateria (id, pregunta, grupo_id, esperadas, desde_mejora, creada) "
                "values (?, ?, ?, ?, ?, ?)",
                [(p["id"], p["pregunta"], p["grupo_id"], json.dumps(p["esperadas"] or []), p["desde_mejora"],
                  p["creada"]) for p in preguntas])
            rotas = nueva.execute("pragma foreign_key_check").fetchall()
            if rotas:
                raise RuntimeError("los datos traídos no cierran entre sí: %s" % rotas[:5])

        print("Traído de Postgres: %d grupos, %d conversaciones, %d notas, %d pedazos de conversación%s, "
              "%d mejoras, %d señales y %d preguntas de la batería."
              % (len(grupos), len(conversaciones), len(notas), len(filas),
                 " (%d repetidos quedaron afuera)" % repetidos if repetidos else "",
                 len(mejoras), len(senales), len(preguntas)))
        print("Las huellas y las palabras se arman solas cuando arranque el servicio.")
        return 0
    finally:
        nueva.close()


if __name__ == "__main__":
    sys.exit(main())
