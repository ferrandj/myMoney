"""Charts as server-side SVG: no JavaScript library, nothing leaves the NAS.

Colours come from the CSS classes s0..s7 (the page's Content-Security-Policy forbids inline styles); hovering a point
shows its values through the native SVG <title> tooltip.
"""

from __future__ import annotations

import math
from datetime import date
from decimal import Decimal
from html import escape

from ..services.analytics import MonthFlow, Series

W, H = 820, 320
LEFT, RIGHT, TOP, BOTTOM = 62, 14, 14, 30
PALETTE_SIZE = 8


def euro(value: float | Decimal, decimals: int = 0) -> str:
    value = float(value)
    text = f"{abs(value):,.{decimals}f}".replace(",", " ").replace(".", ",")
    return f"{'−' if value < 0 else ''}{text} €"


def _axis(value: float) -> str:
    if abs(value) >= 100_000:
        return f"{value / 1000:.0f}\u202fk€".replace("-", "−")
    return f"{value:,.0f}\u202f€".replace(",", "\u202f").replace("-", "−")


def _nice_ticks(low: float, high: float, count: int = 5) -> list[float]:
    if high == low:
        high, low = high + 1, low - 1
    raw = (high - low) / count
    magnitude = 10 ** math.floor(math.log10(raw))
    step = next(m * magnitude for m in (1, 2, 2.5, 5, 10) if m * magnitude >= raw)
    value = math.floor(low / step) * step
    ticks = [round(value, 6)]
    while ticks[-1] < high:  # the last tick must reach the highest value, or the chart would clip it
        value += step
        ticks.append(round(value, 6))
    return ticks


def _fr(day: date) -> str:
    return day.strftime("%d/%m/%Y")


def _frame(content: str, label: str) -> str:
    return f'<svg class="chart-svg" viewBox="0 0 {W} {H}" role="img" aria-label="{escape(label)}">{content}</svg>'


def _grid(ticks: list[float], y) -> str:
    return "".join(
        f'<line class="grid" x1="{LEFT}" x2="{W - RIGHT}" y1="{y(t):.1f}" y2="{y(t):.1f}"/>'
        f'<text class="axis" x="{LEFT - 8}" y="{y(t) + 4:.1f}" text-anchor="end">{_axis(t)}</text>'
        for t in ticks
    )


def line_chart(series: list[Series], label: str = "Évolution") -> str:
    """One line per series over a shared time axis, with a hover band per date."""
    points = [p for s in series for p in s.points]
    if len({d for d, _ in points}) < 2:
        return ""
    days = sorted({d for d, _ in points})
    values = [float(v) for _, v in points]
    ticks = _nice_ticks(min(values), max(values))
    low, high = ticks[0], ticks[-1]
    x0, x1 = days[0].toordinal(), days[-1].toordinal()
    span = (x1 - x0) or 1

    def x(day: date) -> float:
        return LEFT + (day.toordinal() - x0) / span * (W - LEFT - RIGHT)

    def y(value: float) -> float:
        return TOP + (high - value) / ((high - low) or 1) * (H - TOP - BOTTOM)

    out = [_grid(ticks, y)]
    if low < 0 < high:
        out.append(f'<line class="zero" x1="{LEFT}" x2="{W - RIGHT}" y1="{y(0):.1f}" y2="{y(0):.1f}"/>')
    labels = [days[round(i * (len(days) - 1) / 5)] for i in range(6)] if len(days) > 5 else days
    for i, d in enumerate(labels):  # the outer labels hug the edges so they are not cut off
        anchor = "start" if i == 0 else "end" if i == len(labels) - 1 else "middle"
        out.append(f'<text class="axis" x="{x(d):.1f}" y="{H - 8}" text-anchor="{anchor}">{d.strftime("%d/%m/%y")}</text>')
    for i, s in enumerate(series):
        coords = " ".join(f"{x(d):.1f},{y(float(v)):.1f}" for d, v in s.points)
        css = f"s{i % PALETTE_SIZE}"
        if len(series) == 1:
            first, last = s.points[0], s.points[-1]
            area = f"{x(first[0]):.1f},{y(low):.1f} {coords} {x(last[0]):.1f},{y(low):.1f}"
            out.append(f'<polygon class="area {css}" points="{area}"/>')
        if len(s.points) == 1:  # an account known on a single day: a dot, since there is no line to draw
            d, v = s.points[0]
            out.append(f'<circle class="dot {css}" cx="{x(d):.1f}" cy="{y(float(v)):.1f}" r="4"><title>{escape(s.label)} : {euro(v)}</title></circle>')
        else:
            out.append(f'<polyline class="line {css}" points="{coords}"/>')
    # Hover: one band per date, whose tooltip lists the value of every line that day.
    lookup = [{d: v for d, v in s.points} for s in series]
    step = (W - LEFT - RIGHT) / max(len(days) - 1, 1)
    for d in days:
        rows = [f"{s.label} : {euro(look[d])}" for s, look in zip(series, lookup) if d in look]
        out.append(
            f'<rect class="hit" x="{x(d) - step / 2:.1f}" y="{TOP}" width="{step:.1f}" height="{H - TOP - BOTTOM}">'
            f"<title>{escape(_fr(d))}\n{escape(chr(10).join(rows))}</title></rect>"
        )
    return _frame("".join(out), label)


def flow_chart(flows: list[MonthFlow], label: str = "Entrées et sorties par mois") -> str:
    """Income and spending side by side for each month."""
    if not flows:
        return ""
    top = max((float(max(f.income, f.expense)) for f in flows), default=0) or 1
    ticks = _nice_ticks(0, top)
    high = ticks[-1]

    def y(value: float) -> float:
        return TOP + (high - value) / high * (H - TOP - BOTTOM)

    group = (W - LEFT - RIGHT) / len(flows)
    width = min(26, group / 2.6)
    out = [_grid(ticks, y)]
    for i, f in enumerate(flows):
        centre = LEFT + group * (i + 0.5)
        tip = f"{f.month.strftime('%m/%Y')}\nEntrées : {euro(f.income)}\nSorties : {euro(f.expense)}\nSolde : {euro(f.net)}"
        for offset, value, css in ((-width, f.income, "pos"), (0, f.expense, "neg")):
            height = (H - BOTTOM) - y(float(value))
            out.append(
                f'<rect class="bar-{css}" x="{centre + offset:.1f}" y="{y(float(value)):.1f}" width="{width:.1f}" height="{max(height, 0):.1f}">'
                f"<title>{escape(tip)}</title></rect>"
            )
        if len(flows) <= 14 or i % math.ceil(len(flows) / 12) == 0:
            out.append(f'<text class="axis" x="{centre:.1f}" y="{H - 8}" text-anchor="middle">{f.month.strftime("%m/%y")}</text>')
    return _frame("".join(out), label)


def donut(items: list[tuple[str, str, Decimal]], label: str = "Répartition") -> str:
    """Ring chart: one arc per item, in the order given (the same order gives the same colours as the legend)."""
    total = sum((float(v) for _, _, v in items), 0.0)
    if not total:
        return ""
    arcs, offset = [], 0.0
    for i, (_, name, value) in enumerate(items):
        pct = float(value) / total * 100
        arcs.append(
            f'<circle class="arc s{i % PALETTE_SIZE}" cx="21" cy="21" r="15.9155" pathLength="100" '
            f'stroke-dasharray="{pct:.3f} {100 - pct:.3f}" stroke-dashoffset="{25 - offset:.3f}"><title>{escape(name)} : {euro(value)} ({pct:.1f} %)</title></circle>'
        )
        offset += pct
    centre = f'<text class="donut-total" x="21" y="22" text-anchor="middle">{escape(euro(total))}</text>'
    return f'<svg class="donut-svg" viewBox="0 0 42 42" role="img" aria-label="{escape(label)}">{"".join(arcs)}{centre}</svg>'
