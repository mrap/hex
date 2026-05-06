#!/usr/bin/env bash
# check-vector-search.sh — verify sqlite-vec is loadable + memory.db has vectors.
#
# Closes the silent-failure where vec_chunks tables exist but the sqlite-vec
# module isn't loaded, so semantic memory search degrades to FTS-only without
# any signal. Discovered 2026-05-06.
#
# Exits 0 on healthy, 1 with a specific failure message on stderr otherwise.

set -uo pipefail

HEX_DIR="${HEX_DIR:-$HOME/hex}"
MEMORY_DB="$HEX_DIR/.hex/memory.db"
HEX_ALERT="${HEX_ALERT:-$HEX_DIR/.hex/scripts/hex-alert.sh}"

if [ ! -f "$MEMORY_DB" ]; then
  echo "check-vector-search: FAIL — memory.db not found at $MEMORY_DB" >&2
  exit 1
fi

# Probe via the same Python the indexer uses, since sqlite3 CLI doesn't load
# extensions by default but the indexer's environment may.
result="$(python3 - "$MEMORY_DB" <<'PYEOF' 2>&1
import sqlite3, sys
db = sys.argv[1]
try:
    conn = sqlite3.connect(db)
    conn.enable_load_extension(True)
except Exception as e:
    print(f"fail:cannot enable extensions: {e}")
    sys.exit(0)

# Try to query vec_chunks. If the vec0 module isn't loaded this raises.
try:
    cur = conn.execute("SELECT COUNT(*) FROM vec_chunks")
    n = cur.fetchone()[0]
    if n == 0:
        print("fail:vec_chunks empty (table exists, no vectors indexed)")
    else:
        print(f"ok:{n} vectors")
except sqlite3.OperationalError as e:
    msg = str(e)
    if "no such module: vec0" in msg:
        # Try loading sqlite-vec; if available, search will work.
        try:
            import sqlite_vec
            sqlite_vec.load(conn)
            cur = conn.execute("SELECT COUNT(*) FROM vec_chunks")
            n = cur.fetchone()[0]
            if n == 0:
                print("fail:sqlite-vec loadable but vec_chunks empty")
            else:
                print(f"ok:{n} vectors (sqlite-vec loaded on demand)")
        except ImportError:
            print("fail:sqlite-vec not installed (pip install sqlite-vec)")
        except Exception as e2:
            print(f"fail:sqlite-vec load error: {e2}")
    elif "no such table" in msg:
        print("fail:vec_chunks table missing — run memory_index.py --full")
    else:
        print(f"fail:sqlite error: {msg}")
PYEOF
)"

case "$result" in
  ok:*)
    echo "check-vector-search: $result"
    exit 0
    ;;
  fail:*)
    err="${result#fail:}"
    echo "check-vector-search: FAIL — $err" >&2
    if [ -x "$HEX_ALERT" ] && [ "${SKIP_ALERT:-0}" != "1" ]; then
      "$HEX_ALERT" ERROR "vector-search" \
        "Memory semantic search degraded — $err. Memory falls back to FTS keyword-only until fixed."
    fi
    exit 1
    ;;
  *)
    echo "check-vector-search: FAIL — unexpected output: $result" >&2
    exit 1
    ;;
esac
