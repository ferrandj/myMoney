"""Play scenarios on both implementations and compare everything they return and store.

    python tools/diff_scenario.py <rust binary> scenario.json ...      (run from python/)
"""

import json
import os
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(__file__))
from diff_parse import first_difference  # noqa: E402


def normalize(v, key=None):
    """What may differ without being a difference: clock times taken during the run (a balance date set to "now"),
    and what SQLAlchemy appends to a database error after its first line (the SQL and its parameters)."""
    if isinstance(v, dict):
        return {k: normalize(x, k) for k, x in v.items()}
    if isinstance(v, list):
        return [normalize(x) for x in v]
    if key == "error" and isinstance(v, str):
        return v.split("\n")[0]
    if key == "balance_date" and isinstance(v, str) and not v.endswith("00:00:00.000000"):
        return v[:10] + " (heure de l'exécution)"
    return v


def main() -> int:
    binary, scripts = sys.argv[1], sys.argv[2:]
    bad = 0
    here = os.path.dirname(os.path.abspath(__file__))
    for script in scripts:
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            py = subprocess.run([sys.executable, os.path.join(here, "scenario.py"), a, script], capture_output=True, text=True)
            rs = subprocess.run([binary, "scenario", b, script], capture_output=True, text=True)
        name = os.path.basename(script)
        if py.returncode or rs.returncode:
            bad += 1
            print(f"FAIL {name}: python={py.returncode} {py.stderr[-800:]} rust={rs.returncode} {rs.stderr[-800:]}")
            continue
        pj, rj = normalize(json.loads(py.stdout)), normalize(json.loads(rs.stdout))
        diffs = []
        for i, (x, y) in enumerate(zip(pj["results"], rj["results"])):
            d = first_difference(x, y, f"results[{i}]({x.get('op')})")
            if d:
                diffs.append(d)
        if len(pj["results"]) != len(rj["results"]):
            diffs.append("results: lengths differ")
        d = first_difference(pj["db"], rj["db"], "db")
        if d:
            diffs.append(d)
        if diffs:
            bad += 1
            print(f"DIFF {name}:")
            for d in diffs[:15]:
                print("   ", d[:600])
        else:
            print(f"OK   {name} ({len(pj['results'])} operations)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
