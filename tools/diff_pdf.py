"""Compare the Rust PDF reader with pdfplumber on a set of PDF files: words (text and boxes) and page text.

    python tools/diff_pdf.py <rust binary> file.pdf ...
"""

import io
import json
import subprocess
import sys

import pdfplumber


def python_words(data: bytes, tol: float):
    with pdfplumber.open(io.BytesIO(data)) as pdf:
        return [[{k: w[k] for k in ("text", "x0", "x1", "top", "bottom", "upright")} for w in page.extract_words(x_tolerance=tol, y_tolerance=2)] for page in pdf.pages]


def python_text(data: bytes) -> str:
    with pdfplumber.open(io.BytesIO(data)) as pdf:
        return "\n".join((page.extract_text() or "") for page in pdf.pages)


def main() -> int:
    binary, files = sys.argv[1], sys.argv[2:]
    bad = 0
    for f in files:
        data = open(f, "rb").read()
        problems = []
        for tol in (1.0, 3.0):
            py = python_words(data, tol)
            out = subprocess.run([binary, "pdf-words", f, str(tol)], capture_output=True)
            if out.returncode != 0:
                problems.append(f"rust failed: {out.stderr.decode()[:300]}")
                break
            rs = json.loads(out.stdout)
            if len(py) != len(rs):
                problems.append(f"pages {len(py)} != {len(rs)}")
                continue
            for pn, (a, b) in enumerate(zip(py, rs)):
                if len(a) != len(b):
                    problems.append(f"tol {tol} page {pn}: {len(a)} words != {len(b)}")
                for i, (wa, wb) in enumerate(zip(a, b)):
                    if wa["text"] != wb["text"] or wa["upright"] != wb["upright"] or any(abs(wa[k] - wb[k]) > 1e-9 for k in ("x0", "x1", "top", "bottom")):
                        problems.append(f"tol {tol} page {pn} word {i}: {wa} != {wb}")
                        break
        pt = python_text(data)
        out = subprocess.run([binary, "pdf-text", f], capture_output=True)
        rt = out.stdout.decode("utf-8", "surrogateescape")
        if pt != rt:
            for i, (x, y) in enumerate(zip(pt, rt)):
                if x != y:
                    problems.append(f"text differs at {i}: {pt[max(0,i-40):i+40]!r} != {rt[max(0,i-40):i+40]!r}")
                    break
            else:
                problems.append(f"text length {len(pt)} != {len(rt)}")
        name = f.split("/")[-1][:50]
        if problems:
            bad += 1
            print(f"DIFF {name}")
            for p in problems[:6]:
                print("    ", p[:400])
        else:
            print(f"ok   {name}")
    print(f"{len(files) - bad}/{len(files)} identical")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
