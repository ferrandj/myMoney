"""From a folder of (made-up) PDFs, write the variants a bank may send or a transfer may damage, to compare the Rust and
the Python readers on them: encrypted with every standard algorithm (with and without a password to open them), with
broken or missing cross-references, with wrong stream lengths. Needs pypdf.

    python tools/gen_pdf_variants.py <folder of PDFs> <output dir>
"""

import os
import re
import sys
from pathlib import Path

from pypdf import PdfReader, PdfWriter

source, out = Path(sys.argv[1]), Path(sys.argv[2])
pdfs = sorted(p for p in source.rglob("*.pdf"))
n = 0
out.mkdir(parents=True, exist_ok=True)


def put(name: str, data: bytes) -> None:
    global n
    n += 1
    folder = out / f"{n:03d}"
    folder.mkdir(parents=True, exist_ok=True)
    (folder / name).write_bytes(data)


def encrypted(path: Path, algorithm: str, user: str) -> bytes:
    writer = PdfWriter(clone_from=PdfReader(path))
    writer.encrypt(user_password=user, owner_password="owner-pw", algorithm=algorithm)
    tmp = out / ".tmp.pdf"
    with open(tmp, "wb") as fh:
        writer.write(fh)
    data = tmp.read_bytes()
    tmp.unlink()
    return data


def damaged(name: str, data: bytes) -> None:
    put("badxref-" + name, re.sub(rb"startxref\s+(\d+)", lambda m: b"startxref\n" + str(int(m.group(1)) + 7).encode(), data))
    put("noxref-" + name, data.replace(b"startxref", b"startxrfe"))
    put("shortlen-" + name, re.sub(rb"/Length (\d+)", lambda m: b"/Length " + str(max(0, int(m.group(1)) - 3)).encode().ljust(len(m.group(1))), data))
    put("both-" + name, re.sub(rb"/Length (\d+)", lambda m: b"/Length " + str(int(m.group(1)) + 5).encode(), data).replace(b"startxref", b"startxrfe"))


for i, path in enumerate(pdfs[::6][:20]):
    for algorithm in ("RC4-40", "RC4-128", "AES-128", "AES-256-R5", "AES-256"):
        put(f"{algorithm}-{path.name}", encrypted(path, algorithm, ""))
        if i % 4 == 0:
            put(f"{algorithm}-user-{path.name}", encrypted(path, algorithm, "secret"))
for path in pdfs[::9][:12]:
    damaged(path.name, path.read_bytes())
for path in pdfs[:3]:
    damaged("AES-128-" + path.name, encrypted(path, "AES-128", ""))
print(f"{n} files")
