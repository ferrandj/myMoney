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


# Adapters that only exist in the Rust version (the Python reference is frozen): they are taken out of the Rust side
# before comparing, so that everything else is still compared to the byte. Their own tests are in src/tests.
RUST_ONLY = {"fortuneo.life_annual_pdf", "fortuneo.investor_profile_pdf", "fortuneo.life_operations_csv", "hellobank.statement_pdf"}
# Adapters whose reading was deliberately improved in Rust (the arbitrage letter: both wordings, unit value before
# units, contract number as account number): detection and scores are still compared, not what they read.
READ_DIVERGED = {"fortuneo.life_arbitrage_pdf"}


def without_diverged_readings(text: str) -> str:
    data = json.loads(text)
    if not isinstance(data, dict) or not any(k in READ_DIVERGED for k in data.get("parsed", {})):
        return text
    data["parsed"] = {k: v for k, v in data["parsed"].items() if k not in READ_DIVERGED}
    return json.dumps(data)


def without_rust_only(text: str) -> str:
    data = json.loads(text)
    if not isinstance(data, dict) or not any(c.get("adapter") in RUST_ONLY for c in data.get("candidates", [])):
        return text
    data["candidates"] = [c for c in data["candidates"] if c.get("adapter") not in RUST_ONLY]
    data["parsed"] = {k: v for k, v in data.get("parsed", {}).items() if k not in RUST_ONLY}
    return json.dumps(data)


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
        py = without_diverged_readings(python_side(f))
        out = subprocess.run([binary, "parse", f], capture_output=True)
        rs = out.stdout.decode("utf-8", "surrogateescape").rstrip("\n")
        if out.returncode == 0:
            try:
                rs = without_diverged_readings(without_rust_only(rs))
            except ValueError:
                pass
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
