#!/bin/sh
# Render tools/jinja_cases.py with Jinja2 and with the Rust engine, and show the cases that differ.
set -e
here=$(dirname "$0")
tmp=${TMPDIR:-/tmp}/jinja_cases.$$
mkdir -p "$tmp"
python3 "$here/jinja_cases.py" dump > "$tmp/cases.json"
(cd "$here/../python" && python3 ../tools/jinja_cases.py render) > "$tmp/py.json"
"${MYMONEY_BIN:-$here/../target/release/mymoney}" jinja-cases "$tmp/cases.json" > "$tmp/rs.json"
python3 - "$tmp/py.json" "$tmp/rs.json" <<'PY'
import json, sys
py = dict(json.load(open(sys.argv[1]))); rs = dict(json.load(open(sys.argv[2])))
bad = 0
for k, v in py.items():
    r = rs.get(k)
    same = v == r or (v.startswith("ERROR") and r and r.startswith("ERROR"))
    if not same:
        bad += 1
        print(f"--- {k}\n py: {v!r}\n rs: {r!r}")
print(f"{len(py) - bad}/{len(py)} identiques")
sys.exit(1 if bad else 0)
PY
