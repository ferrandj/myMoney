//! Charts as server-side SVG, computed and written exactly as the Python version does (the same floats, rounded and
//! formatted the same way), so the pages are identical.

use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;

use crate::py::{self, float_fixed, std_html_escape as escape, PyDec};
use crate::services::analytics::{MonthFlow, Series};

const W: f64 = 820.0;
const H: f64 = 320.0;
const LEFT: f64 = 62.0;
const RIGHT: f64 = 14.0;
const TOP: f64 = 14.0;
const BOTTOM: f64 = 30.0;
pub const PALETTE_SIZE: usize = 8;
const CHAR_W: f64 = 6.6;
const LINE_H: f64 = 16.0;
const PAD: f64 = 9.0;

fn f1(x: f64) -> String {
    format!("{x:.1}")
}

fn dec_f64(d: &Decimal) -> f64 {
    PyDec::from(d).to_f64()
}

/// `round(x, n)` on a float.
fn round_to(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

pub fn euro(value: f64, decimals: usize) -> String {
    let text = float_fixed(value.abs(), decimals, false, true).replace(',', " ").replace('.', ",");
    format!("{}{text} €", if value < 0.0 { "−" } else { "" })
}

fn axis(value: f64) -> String {
    if value.abs() >= 100_000.0 {
        return format!("{}\u{202f}k€", float_fixed(value / 1000.0, 0, false, false)).replace('-', "−");
    }
    format!("{}\u{202f}€", float_fixed(value, 0, false, true)).replace(',', "\u{202f}").replace('-', "−")
}

pub(crate) fn nice_ticks(low: f64, high: f64, count: f64) -> Vec<f64> {
    let (low, high) = if high == low { (low - 1.0, high + 1.0) } else { (low, high) };
    let raw = (high - low) / count;
    let magnitude = 10f64.powi(raw.log10().floor() as i32);
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * magnitude).find(|s| *s >= raw).unwrap_or(10.0 * magnitude);
    let mut value = (low / step).floor() * step;
    let mut ticks = vec![round_to(value, 6)];
    while *ticks.last().unwrap() < high {
        value += step;
        ticks.push(round_to(value, 6));
    }
    ticks
}

fn fr(day: NaiveDate) -> String {
    day.format("%d/%m/%Y").to_string()
}

fn frame(content: &str, label: &str) -> String {
    format!(r#"<svg class="chart-svg" viewBox="0 0 {W} {H}" role="img" aria-label="{}">{content}</svg>"#, escape(label))
}

fn grid(ticks: &[f64], y: &dyn Fn(f64) -> f64) -> String {
    ticks
        .iter()
        .map(|t| {
            format!(
                r#"<line class="grid" x1="{LEFT}" x2="{}" y1="{}" y2="{}"/><text class="axis" x="{}" y="{}" text-anchor="end">{}</text>"#,
                W - RIGHT,
                f1(y(*t)),
                f1(y(*t)),
                LEFT - 8.0,
                f1(y(*t) + 4.0),
                axis(*t)
            )
        })
        .collect()
}

/// The tooltip box: the date, then one line per value (with the colour of its series when it has one).
fn tip(cx: f64, head: &str, rows: &[(String, Option<String>)]) -> String {
    let chars = rows.iter().map(|(t, css)| t.chars().count() + if css.is_some() { 2 } else { 0 }).chain(std::iter::once(head.chars().count())).max().unwrap();
    let width = chars as f64 * CHAR_W + 2.0 * PAD;
    let height = (rows.len() as f64 + 1.0) * LINE_H + PAD;
    let left = if cx + 14.0 + width <= W - 4.0 { cx + 14.0 } else { (cx - 14.0 - width).max(4.0) };
    let top = TOP + 6.0;
    let mut out = vec![format!(r#"<rect class="tip-box" x="{}" y="{top}" width="{}" height="{}" rx="6"/>"#, f1(left), f1(width), f1(height))];
    out.push(format!(r#"<text class="tip-head" x="{}" y="{}">{}</text>"#, f1(left + PAD), f1(top + PAD + 6.0), escape(head)));
    for (i, (text, css)) in rows.iter().enumerate() {
        let y = top + PAD + 6.0 + (i as f64 + 1.0) * LINE_H;
        let mut shift = 0.0;
        if let Some(css) = css {
            out.push(format!(r#"<circle class="dot {css}" cx="{}" cy="{}" r="4"/>"#, f1(left + PAD + 4.0), f1(y - 4.0)));
            shift = 14.0;
        }
        out.push(format!(r#"<text class="tip-row" x="{}" y="{}">{}</text>"#, f1(left + PAD + shift), f1(y), escape(text)));
    }
    out.concat()
}

fn ordinal(d: NaiveDate) -> i64 {
    d.num_days_from_ce() as i64
}

/// One line per series over a shared time axis, with a hover band per date.
pub fn line_chart(series: &[Series], label: &str) -> String {
    let points: Vec<&(NaiveDate, Decimal)> = series.iter().flat_map(|s| s.points.iter()).collect();
    let mut days: Vec<NaiveDate> = points.iter().map(|(d, _)| *d).collect();
    days.sort();
    days.dedup();
    if days.len() < 2 {
        return String::new();
    }
    let values: Vec<f64> = points.iter().map(|(_, v)| dec_f64(v)).collect();
    let ticks = nice_ticks(values.iter().cloned().fold(f64::INFINITY, f64::min), values.iter().cloned().fold(f64::NEG_INFINITY, f64::max), 5.0);
    let (low, high) = (ticks[0], *ticks.last().unwrap());
    let (x0, x1) = (ordinal(days[0]), ordinal(*days.last().unwrap()));
    let span = if x1 - x0 != 0 { x1 - x0 } else { 1 };
    let x = |day: NaiveDate| LEFT + (ordinal(day) - x0) as f64 / span as f64 * (W - LEFT - RIGHT);
    let y = |value: f64| TOP + (high - value) / if high - low != 0.0 { high - low } else { 1.0 } * (H - TOP - BOTTOM);
    let mut out = vec![grid(&ticks, &y)];
    if low < 0.0 && 0.0 < high {
        out.push(format!(r#"<line class="zero" x1="{LEFT}" x2="{}" y1="{}" y2="{}"/>"#, W - RIGHT, f1(y(0.0)), f1(y(0.0))));
    }
    let labels: Vec<NaiveDate> =
        if days.len() > 5 { (0..6).map(|i| days[py::py_round(i as f64 * (days.len() - 1) as f64 / 5.0) as usize]).collect() } else { days.clone() };
    for (i, d) in labels.iter().enumerate() {
        let anchor = if i == 0 {
            "start"
        } else if i == labels.len() - 1 {
            "end"
        } else {
            "middle"
        };
        out.push(format!(r#"<text class="axis" x="{}" y="{}" text-anchor="{anchor}">{}</text>"#, f1(x(*d)), H - 8.0, d.format("%d/%m/%y")));
    }
    for (i, s) in series.iter().enumerate() {
        let coords = s.points.iter().map(|(d, v)| format!("{},{}", f1(x(*d)), f1(y(dec_f64(v))))).collect::<Vec<_>>().join(" ");
        let css = format!("s{}", i % PALETTE_SIZE);
        if series.len() == 1 {
            let (first, last) = (s.points[0], *s.points.last().unwrap());
            let area = format!("{},{} {coords} {},{}", f1(x(first.0)), f1(y(low)), f1(x(last.0)), f1(y(low)));
            out.push(format!(r#"<polygon class="area {css}" points="{area}"/>"#));
        }
        if s.points.len() == 1 {
            let (d, v) = s.points[0];
            out.push(format!(r#"<circle class="dot {css}" cx="{}" cy="{}" r="4"/>"#, f1(x(d)), f1(y(dec_f64(&v)))));
        } else {
            out.push(format!(r#"<polyline class="line {css}" points="{coords}"/>"#));
        }
    }
    let lookup: Vec<std::collections::HashMap<NaiveDate, Decimal>> = series.iter().map(|s| s.points.iter().cloned().collect()).collect();
    let step = (W - LEFT - RIGHT) / (days.len() as f64 - 1.0).max(1.0);
    for d in &days {
        let shown: Vec<(usize, &Series, Decimal)> = series.iter().enumerate().filter_map(|(i, s)| lookup[i].get(d).map(|v| (i, s, *v))).collect();
        let mut rows: Vec<(String, Option<String>)> =
            shown.iter().map(|(i, s, v)| (format!("{} : {}", s.label, euro(dec_f64(v), 0)), Some(format!("s{}", i % PALETTE_SIZE)))).collect();
        if shown.len() > 1 {
            let total = shown.iter().fold(PyDec::int(0), |a, (_, _, v)| a.add(&PyDec::from(v)));
            rows.push((format!("Total : {}", euro(total.to_f64(), 0)), None));
        }
        let marks: String = shown
            .iter()
            .map(|(i, _, v)| format!(r#"<circle class="dot s{}" cx="{}" cy="{}" r="4"/>"#, i % PALETTE_SIZE, f1(x(*d)), f1(y(dec_f64(v)))))
            .collect();
        out.push(format!(
            r#"<rect class="hit" x="{}" y="{TOP}" width="{}" height="{}"/><g class="pop"><line class="guide" x1="{}" x2="{}" y1="{TOP}" y2="{}"/>{marks}{}</g>"#,
            f1(x(*d) - step / 2.0),
            f1(step),
            H - TOP - BOTTOM,
            f1(x(*d)),
            f1(x(*d)),
            H - BOTTOM,
            tip(x(*d), &fr(*d), &rows)
        ));
    }
    frame(&out.concat(), label)
}

/// Income and spending side by side for each month.
pub fn flow_chart(flows: &[MonthFlow], label: &str) -> String {
    if flows.is_empty() {
        return String::new();
    }
    let top = flows.iter().map(|f| dec_f64(&f.income.max(f.expense))).fold(f64::NEG_INFINITY, f64::max);
    let top = if top == 0.0 { 1.0 } else { top };
    let ticks = nice_ticks(0.0, top, 5.0);
    let high = *ticks.last().unwrap();
    let y = |value: f64| TOP + (high - value) / high * (H - TOP - BOTTOM);
    let group = (W - LEFT - RIGHT) / flows.len() as f64;
    let width = 26f64.min(group / 2.6);
    let mut out = vec![grid(&ticks, &y)];
    let mut hovers = vec![];
    let every = (flows.len() as f64 / 12.0).ceil() as usize;
    for (i, f) in flows.iter().enumerate() {
        let centre = LEFT + group * (i as f64 + 0.5);
        let rows = vec![
            (format!("Entrées : {}", euro(dec_f64(&f.income), 0)), None),
            (format!("Sorties : {}", euro(dec_f64(&f.expense), 0)), None),
            (format!("Solde : {}", euro(dec_f64(&f.net()), 0)), None),
        ];
        for (offset, value, css) in [(-width, f.income, "pos"), (0.0, f.expense, "neg")] {
            let height = (H - BOTTOM) - y(dec_f64(&value));
            out.push(format!(
                r#"<rect class="bar-{css}" x="{}" y="{}" width="{}" height="{}"/>"#,
                f1(centre + offset),
                f1(y(dec_f64(&value))),
                f1(width),
                f1(height.max(0.0))
            ));
        }
        hovers.push(format!(
            r#"<rect class="hit" x="{}" y="{TOP}" width="{}" height="{}"/><g class="pop">{}</g>"#,
            f1(centre - group / 2.0),
            f1(group),
            H - TOP - BOTTOM,
            tip(centre, &f.month.format("%m/%Y").to_string(), &rows)
        ));
        if flows.len() <= 14 || i % every == 0 {
            out.push(format!(r#"<text class="axis" x="{}" y="{}" text-anchor="middle">{}</text>"#, f1(centre), H - 8.0, f.month.format("%m/%y")));
        }
    }
    out.extend(hovers);
    frame(&out.concat(), label)
}

/// One bar per month (spending, or income), with the month and the amount in a tooltip.
pub fn month_bars(points: &[(NaiveDate, Decimal)], label: &str, css: &str) -> String {
    if points.is_empty() || !points.iter().any(|(_, v)| !v.is_zero()) {
        return String::new();
    }
    let max = points.iter().map(|(_, v)| dec_f64(v)).fold(f64::NEG_INFINITY, f64::max);
    let ticks = nice_ticks(0.0, if max == 0.0 { 1.0 } else { max }, 5.0);
    let high = *ticks.last().unwrap();
    let y = |value: f64| TOP + (high - value) / high * (H - TOP - BOTTOM);
    let group = (W - LEFT - RIGHT) / points.len() as f64;
    let width = 34f64.min(group * 0.7);
    let mut out = vec![grid(&ticks, &y)];
    let mut hovers = vec![];
    let every = (points.len() as f64 / 12.0).ceil() as usize;
    for (i, (month, value)) in points.iter().enumerate() {
        let centre = LEFT + group * (i as f64 + 0.5);
        let top = y(dec_f64(value));
        out.push(format!(
            r#"<rect class="bar-{css}" x="{}" y="{}" width="{}" height="{}"/>"#,
            f1(centre - width / 2.0),
            f1(top),
            f1(width),
            f1(((H - BOTTOM) - top).max(0.0))
        ));
        hovers.push(format!(
            r#"<rect class="hit" x="{}" y="{TOP}" width="{}" height="{}"/><g class="pop">{}</g>"#,
            f1(centre - group / 2.0),
            f1(group),
            H - TOP - BOTTOM,
            tip(centre, &month.format("%m/%Y").to_string(), &[(euro(dec_f64(value), 0), None)])
        ));
        if points.len() <= 14 || i % every == 0 {
            out.push(format!(r#"<text class="axis" x="{}" y="{}" text-anchor="middle">{}</text>"#, f1(centre), H - 8.0, month.format("%m/%y")));
        }
    }
    frame(&(out.concat() + &hovers.concat()), label)
}

/// Ring chart: one arc per item, in the order given (the same order gives the same colours as the legend).
pub fn donut(items: &[(String, String, Decimal)], label: &str) -> String {
    let total = items.iter().fold(0.0, |a, (_, _, v)| a + dec_f64(v));
    if total == 0.0 {
        return String::new();
    }
    let mut arcs = String::new();
    let mut offset = 0.0;
    for (i, (_, name, value)) in items.iter().enumerate() {
        let pct = dec_f64(value) / total * 100.0;
        arcs.push_str(&format!(
            r#"<circle class="arc s{}" cx="21" cy="21" r="15.9155" pathLength="100" stroke-dasharray="{pct:.3} {:.3}" stroke-dashoffset="{:.3}"><title>{} : {} ({pct:.1} %)</title></circle>"#,
            i % PALETTE_SIZE,
            100.0 - pct,
            25.0 - offset,
            escape(name),
            euro(dec_f64(value), 0)
        ));
        offset += pct;
    }
    let centre = format!(r#"<text class="donut-total" x="21" y="22" text-anchor="middle">{}</text>"#, escape(&euro(total, 0)));
    format!(r#"<svg class="donut-svg" viewBox="0 0 42 42" role="img" aria-label="{}">{arcs}{centre}</svg>"#, escape(label))
}

/// The net worth history on the dashboard.
pub fn sparkline(points: &[(NaiveDate, Decimal)]) -> String {
    let (width, height) = (640.0, 140.0);
    if points.len() < 2 {
        return String::new();
    }
    let values: Vec<f64> = points.iter().map(|(_, v)| dec_f64(v)).collect();
    let low = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let high = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = if high - low != 0.0 { high - low } else { 1.0 };
    let step = width / (values.len() as f64 - 1.0);
    let line = values
        .iter()
        .enumerate()
        .map(|(i, v)| format!("{},{}", f1(i as f64 * step), f1(height - 8.0 - (v - low) / span * (height - 16.0))))
        .collect::<Vec<_>>()
        .join(" ");
    let area = format!("0,{height} {line} {width},{height}");
    format!(
        r#"<svg class="spark" viewBox="0 0 {width} {height}" preserveAspectRatio="none" role="img" aria-label="Évolution du patrimoine"><polygon class="spark-area" points="{area}"/><polyline class="spark-line" points="{line}"/></svg>"#
    )
}
