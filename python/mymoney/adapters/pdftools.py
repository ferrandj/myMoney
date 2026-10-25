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


_GROUP = re.compile(r"^\d{3}$")
_LAST_GROUP = re.compile(r"^\d{3},\d{2}$")
_THOUSANDS_GAP = 4.0  # points: the space inside an amount is narrower than the one between two words


def join_thousands(row: list[dict]) -> list[dict]:
    """Glue "2" + "000,00" back into "2 000,00". Read with a small tolerance, the space that separates the thousands splits
    an amount in two words, and the first part would otherwise end up in the label (or be lost)."""
    out: list[dict] = []
    i = 0
    while i < len(row):
        w = row[i]
        if re.fullmatch(r"\d{1,3}", w["text"]):
            group, j, end = [w["text"]], i + 1, w
            while j < len(row) and row[j]["x0"] - end["x1"] < _THOUSANDS_GAP and (_GROUP.match(row[j]["text"]) or _LAST_GROUP.match(row[j]["text"])):
                group.append(row[j]["text"])
                end = row[j]
                j += 1
                if _LAST_GROUP.match(end["text"]):
                    break
            if len(group) > 1 and _LAST_GROUP.match(group[-1]):
                out.append({**w, "text": " ".join(group), "x1": end["x1"]})
                i = j
                continue
        out.append(w)
        i += 1
    return out


def lines(words: list[dict], tolerance: float = 3.0, join_amounts: bool = False) -> list[list[dict]]:
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
    return [join_thousands(row) for row in rows] if join_amounts else rows
