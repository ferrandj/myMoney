"""Compare what the Rust and the Python readers make of documents: identification, then every candidate's reading.

    python tools/diff_parse.py <rust binary> file ...      (run from python/, so that `mymoney` is importable)

The Rust side is `mymoney parse <file>`; both sides are serialized with json.dumps, and must be equal to the byte.
"""

import json
import os
import subprocess
import sys

from mymoney.adapters import SourceDocument, identify, parse_with


def python_side(path: str) -> str:
    data = open(path, "rb").read()
    doc = SourceDocument(os.path.basename(path), data)
    found = identify(doc)
    out = {
        "decision": found.decision,
        "message": found.message,
        "institution": found.institution,
        "candidates": [c.to_json() for c in found.candidates],
    }
    parsed = {}
    for c in found.candidates:
        try:
            parsed[c.adapter.id] = [st.to_json() for st in parse_with(c.adapter.id, doc)]
        except Exception as exc:  # the message is what the user sees: it must be the same too
            parsed[c.adapter.id] = {"error": str(exc)}
    out["parsed"] = parsed
    return json.dumps(out)


def first_difference(a, b, path="$"):
    if type(a) is not type(b):
        return f"{path}: {a!r} != {b!r}"
    if isinstance(a, dict):
        if list(a) != list(b):
            return f"{path}: keys {list(a)} != {list(b)}"
        for k in a:
            d = first_difference(a[k], b[k], f"{path}.{k}")
            if d:
                return d
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return f"{path}: {len(a)} items != {len(b)}"
        for i, (x, y) in enumerate(zip(a, b)):
            d = first_difference(x, y, f"{path}[{i}]")
            if d:
                return d
        return None
    return None if a == b else f"{path}: {a!r} != {b!r}"


# The one difference left, on purpose: an amount written with a positive exponent ("1e2", which no bank writes) is
# Decimal('1E+2') for Python and 100 for Rust (rust_decimal has no positive exponent). Same value, same page.
KNOWN = {
    "big_amount.ofx": "$.parsed.generic.ofx[0].transactions[1].amount: '1E+2' != '100'",
}


def main() -> int:
    binary, files = sys.argv[1], sys.argv[2:]
    bad = 0
    for f in files:
        py = python_side(f)
        out = subprocess.run([binary, "parse", f], capture_output=True)
        rs = out.stdout.decode("utf-8", "surrogateescape").rstrip("\n")
        name = os.path.basename(f)[:60]
        if out.returncode != 0:
            bad += 1
            print(f"FAIL {name}: {out.stderr.decode()[-500:]}")
        elif py != rs:
            bad += 1
            try:
                why = first_difference(json.loads(py), json.loads(rs))
            except ValueError as e:
                why = f"invalid JSON from Rust: {e}"
            if KNOWN.get(os.path.basename(f)) == why:
                bad -= 1
                print(f"KNOWN {name}: {why}")
                continue
            print(f"DIFF {name}: {why or 'same values, different serialization'}")
    print(f"{len(files) - bad}/{len(files)} identical")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
