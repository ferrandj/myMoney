"""PDF reading shared by adapters: text, words with positions, and repair of mangled accents."""

from __future__ import annotations

import io
import re

_CID = re.compile(r"\(cid:(\d+)\)")
# Some banks embed their font without a proper Unicode map: pdfminer then returns the glyph codes of the font.
# Seen on real statements: é comes out as Ø, è as Ł, and à, ô, °, € as (cid:224), (cid:244), (cid:176), (cid:128).
_GLYPHS = str.maketrans({"Ø": "é", "Ł": "è"})


def fix_text(text: str) -> str:
    text = _CID.sub(lambda m: bytes([int(m.group(1))]).decode("cp1252", "replace") if 32 <= int(m.group(1)) < 256 else "", text)
    return text.translate(_GLYPHS)


def _open(data: bytes):
    import pdfplumber
    from pdfminer.pdfdocument import PDFPasswordIncorrect

    try:
        return pdfplumber.open(io.BytesIO(data))
    except PDFPasswordIncorrect:
        raise ValueError("Ce PDF est protégé par un mot de passe : enregistre-le sans protection puis dépose-le à nouveau") from None


def extract_text(data: bytes) -> str:
    with _open(data) as pdf:
        return fix_text("\n".join((page.extract_text() or "") for page in pdf.pages))


def extract_words(data: bytes, x_tolerance: float = 1.0) -> list[list[dict]]:
    """One list of words per page. The tolerance is the gap that separates two words: 3 glues, 1 splits cleanly."""
    with _open(data) as pdf:
        pages = []
        for page in pdf.pages:
            words = page.extract_words(x_tolerance=x_tolerance, y_tolerance=2)
            for w in words:
                w["text"] = fix_text(w["text"])
            pages.append(words)
        return pages


def lines(words: list[dict], tolerance: float = 3.0) -> list[list[dict]]:
    """Group the words of a page into visual lines (top to bottom, left to right)."""
    ordered = sorted(words, key=lambda w: (w["top"], w["x0"]))
    rows: list[list[dict]] = []
    for w in ordered:
        if rows and abs(rows[-1][0]["top"] - w["top"]) <= tolerance:
            rows[-1].append(w)
        else:
            rows.append([w])
    for row in rows:
        row.sort(key=lambda w: w["x0"])
    return rows
