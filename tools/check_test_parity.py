#!/usr/bin/env python3
"""Every test of the Python suite has its counterpart in the Rust suite.

The Rust tests are ported one by one from python/tests (same documents, same checks) in src/tests/<file>.rs, with the
same names. A parametrized Python test becomes one Rust test per case, named `<python name>_<case>`.

    python tools/check_test_parity.py           # the Python tests are collected by pytest
"""
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

root = Path(__file__).resolve().parent.parent
collected = subprocess.run([sys.executable, "-m", "pytest", "--collect-only", "-q"], cwd=root / "python", capture_output=True, text=True).stdout
python = Counter()  # (module, test name) -> number of cases
for line in collected.splitlines():
    found = re.match(r"tests/test_(\w+)\.py::(\w+)", line)
    if found:
        python[(found.group(1), found.group(2))] += 1

rust = Counter()  # (module, test name) -> 1
for path in sorted((root / "src" / "tests").glob("*.rs")):
    if path.name == "mod.rs":
        continue
    text = path.read_text(encoding="utf-8")
    names = re.findall(r"#\[test\]\s*(?:#\[[^\]]*\]\s*)*(?:pub )?fn (\w+)", text)
    names += re.findall(r"^\s+(test_\w+)\s*:\s*[(\"]", text, re.M)  # tests written by a macro
    for name in names:
        rust[(path.stem, name)] += 1

missing = []
for (module, name), cases in sorted(python.items()):
    have = sum(n for (m, r), n in rust.items() if m == module and (r == name or r.startswith(name + "_")))
    if have < cases:
        missing.append(f"{module}: {name} ({have} of {cases})")
total_python, total_rust = sum(python.values()), sum(rust.values())
print(f"Python: {total_python} tests, Rust (src/tests): {total_rust} tests")
if missing:
    print("Missing in Rust:\n  " + "\n  ".join(missing))
    sys.exit(1)
print("Every Python test has its Rust counterpart.")
