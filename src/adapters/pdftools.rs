//! PDF reading shared by adapters: text, words with positions, and repair of mangled accents.

use super::base::{cp1252_char, W};
use super::re::{fullmatch, py_match, re};

/// Some banks embed their font without a proper Unicode map: pdfminer then returns the glyph codes of the font.
/// Seen on real statements: é comes out as Ø, è as Ł, and à, ô, °, € as (cid:224), (cid:244), (cid:176), (cid:128).
pub fn fix_text(text: &str) -> String {
    let replaced = re(r"\(cid:(\d+)\)").replace_all(text, |c: &regex::Captures| {
        let n: i64 = c[1].parse().unwrap_or(-1);
        if (32..256).contains(&n) {
            cp1252_char(n as u8).map(|ch| ch.to_string()).unwrap_or_else(|| "\u{fffd}".to_string())
        } else {
            String::new()
        }
    });
    replaced.chars().map(|c| match c {
        'Ø' => 'é',
        'Ł' => 'è',
        other => other,
    }).collect()
}

fn pdf_error(e: crate::pdf::Error) -> String {
    match e {
        // pdfplumber wraps pdfminer's PDFPasswordIncorrect (an empty message): the Python version's own message for it
        // is never reached
        crate::pdf::Error::Password => String::new(),
        crate::pdf::Error::Invalid(s) => s,
    }
}

pub fn extract_text(data: &[u8]) -> Result<String, String> {
    Ok(fix_text(&crate::pdf::extract_text(data).map_err(pdf_error)?))
}

/// One list of words per page. The tolerance is the gap that separates two words: 3 glues, 1 splits cleanly.
pub fn extract_words(data: &[u8], x_tolerance: f64) -> Result<Vec<Vec<W>>, String> {
    let pages = crate::pdf::extract_words(data, x_tolerance).map_err(pdf_error)?;
    Ok(pages
        .into_iter()
        .map(|p| p.into_iter().map(|w| W { text: fix_text(&w.text), x0: w.x0, x1: w.x1, top: w.top, bottom: w.bottom }).collect())
        .collect())
}

const THOUSANDS_GAP: f64 = 4.0; // points: the space inside an amount is narrower than the one between two words

/// Glue "2" + "000,00" back into "2 000,00".
pub fn join_thousands(row: Vec<W>) -> Vec<W> {
    let group_re = |t: &str| py_match(r"^\d{3}$", t).is_some();
    let last_re = |t: &str| py_match(r"^\d{3},\d{2}$", t).is_some();
    let mut out: Vec<W> = Vec::new();
    let mut i = 0;
    while i < row.len() {
        let w = &row[i];
        if fullmatch(r"\d{1,3}", &w.text).is_some() {
            let mut group = vec![w.text.clone()];
            let mut j = i + 1;
            let mut end = w.clone();
            while j < row.len() && row[j].x0 - end.x1 < THOUSANDS_GAP && (group_re(&row[j].text) || last_re(&row[j].text)) {
                group.push(row[j].text.clone());
                end = row[j].clone();
                j += 1;
                if last_re(&end.text) {
                    break;
                }
            }
            if group.len() > 1 && last_re(group.last().unwrap()) {
                out.push(W { text: group.join(" "), x1: end.x1, ..w.clone() });
                i = j;
                continue;
            }
        }
        out.push(w.clone());
        i += 1;
    }
    out
}

/// Group the words of a page into visual lines (top to bottom, left to right).
pub fn lines(words: &[W], tolerance: f64, join_amounts: bool) -> Vec<Vec<W>> {
    let mut ordered: Vec<W> = words.to_vec();
    ordered.sort_by(|a, b| a.top.partial_cmp(&b.top).unwrap().then(a.x0.partial_cmp(&b.x0).unwrap()));
    let mut rows: Vec<Vec<W>> = Vec::new();
    for w in ordered {
        match rows.last_mut() {
            Some(r) if (r[0].top - w.top).abs() <= tolerance => r.push(w),
            _ => rows.push(vec![w]),
        }
    }
    for r in rows.iter_mut() {
        r.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap());
    }
    if join_amounts {
        rows.into_iter().map(join_thousands).collect()
    } else {
        rows
    }
}

/// The text of a row of words, joined by spaces.
pub fn row_text(row: &[W]) -> String {
    row.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
}
