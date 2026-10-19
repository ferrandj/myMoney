from __future__ import annotations

from .common import ParsedStatement


def detect_format(filename: str, data: bytes) -> str:
    name = filename.lower()
    head = data[:2048].lstrip()
    if head.startswith(b"%PDF"):
        return "pdf"
    if name.endswith((".ofx", ".qfx")) or b"OFXHEADER" in head or b"<OFX>" in head.upper():
        return "ofx"
    if name.endswith(".qif") or head.startswith(b"!Type"):
        return "qif"
    if name.endswith((".csv", ".txt", ".tsv")):
        return "csv"
    return "unknown"


def parse_document(filename: str, data: bytes) -> list[ParsedStatement]:
    fmt = detect_format(filename, data)
    if fmt == "pdf":
        from .pdf_importer import parse_pdf

        return parse_pdf(data, filename)
    if fmt == "ofx":
        from .ofx_importer import parse_ofx

        return parse_ofx(data, filename)
    if fmt == "qif":
        from .ofx_importer import parse_qif

        return parse_qif(data, filename)
    if fmt == "csv":
        from .csv_importer import parse_csv

        return parse_csv(data, filename)
    raise ValueError("Format non pris en charge (PDF, CSV, OFX/QFX ou QIF attendu)")
