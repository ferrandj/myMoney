//! pdfplumber's word and text extraction (`WordExtractor`, `cluster_objects`, `extract_text`), over the
//! characters of a page.

use super::interp::Char;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Word {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
    pub top: f64,
    pub bottom: f64,
    pub upright: bool,
}

/// Python's `str.isspace()` (false for the empty string).
pub fn py_isspace(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            matches!(
                c,
                '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c' | '\x1d' | '\x1e' | '\x1f' | ' ' | '\u{85}' | '\u{a0}' | '\u{1680}'
                    | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
            )
        })
}

fn ligature(t: &str) -> &str {
    match t {
        "ﬀ" => "ff",
        "ﬃ" => "ffi",
        "ﬄ" => "ffl",
        "ﬁ" => "fi",
        "ﬂ" => "fl",
        "ﬆ" => "st",
        "ﬅ" => "st",
        other => other,
    }
}

fn cmp_f(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

/// `cluster_list` + `make_cluster_dict` + `cluster_objects`: groups of items whose key values chain within
/// `tolerance` (distinct values sorted, a gap larger than the tolerance starts a new group).
fn cluster_objects<T: Clone>(items: &[T], key: impl Fn(&T) -> f64, tolerance: f64, preserve_order: bool) -> Vec<Vec<T>> {
    let mut xs: Vec<f64> = items.iter().map(&key).collect();
    xs.sort_by(|a, b| cmp_f(*a, *b));
    xs.dedup_by(|a, b| *a == *b);
    let mut groups = Vec::with_capacity(xs.len());
    if tolerance == 0.0 || xs.len() < 2 {
        groups.extend(0..xs.len());
    } else {
        let mut g = 0usize;
        groups.push(0);
        for i in 1..xs.len() {
            if xs[i] > xs[i - 1] + tolerance {
                g += 1;
            }
            groups.push(g);
        }
    }
    let lookup = |v: f64| {
        let i = xs.partition_point(|x| *x < v);
        groups.get(i).copied().unwrap_or(0)
    };
    let mut tuples: Vec<(T, usize)> = items.iter().map(|x| (x.clone(), lookup(key(x)))).collect();
    if !preserve_order {
        tuples.sort_by_key(|t| t.1); // stable, like Python's sorted
    }
    let mut out: Vec<Vec<T>> = Vec::new();
    let mut current: Option<usize> = None;
    for (x, c) in tuples {
        if current == Some(c) {
            out.last_mut().unwrap().push(x);
        } else {
            out.push(vec![x]);
            current = Some(c);
        }
    }
    out
}

fn begins_new_word(prev: &Char, curr: &Char, ltr: bool, x_tol: f64, y_tol: f64) -> bool {
    let (x, y, ay, cy, ax, bx, cx);
    if ltr {
        x = x_tol;
        y = y_tol;
        ay = prev.top;
        cy = curr.top;
        ax = prev.x0;
        bx = prev.x1;
        cx = curr.x0;
    } else {
        // "ttb": rotated text
        x = y_tol;
        y = x_tol;
        ay = prev.x0;
        cy = curr.x0;
        ax = prev.top;
        bx = prev.bottom;
        cx = curr.top;
    }
    cx < ax || cx > bx + x || (cy - ay).abs() > y
}

fn merge(chars: &[Char]) -> Word {
    let mut x0 = chars[0].x0;
    let mut top = chars[0].top;
    let mut x1 = chars[0].x1;
    let mut bottom = chars[0].bottom;
    for c in &chars[1..] {
        if c.x0 < x0 {
            x0 = c.x0;
        }
        if c.top < top {
            top = c.top;
        }
        if c.x1 > x1 {
            x1 = c.x1;
        }
        if c.bottom > bottom {
            bottom = c.bottom;
        }
    }
    let text = chars.iter().map(|c| ligature(&c.text)).collect::<String>();
    Word { text, x0, x1, top, bottom, upright: chars[0].upright }
}

/// `WordExtractor(x_tolerance, y_tolerance).iter_extract_tuples(chars)`: (word, its characters).
fn extract_tuples(chars: &[Char], x_tol: f64, y_tol: f64) -> Vec<(Word, Vec<Char>)> {
    let mut out = Vec::new();
    // itertools.groupby on "upright": consecutive runs only.
    let mut runs: Vec<Vec<Char>> = Vec::new();
    for c in chars {
        match runs.last_mut() {
            Some(r) if r[0].upright == c.upright => r.push(c.clone()),
            _ => runs.push(vec![c.clone()]),
        }
    }
    for run in runs {
        let upright = run[0].upright;
        let lines = if upright {
            cluster_objects(&run, |c| c.top, y_tol, false)
        } else {
            cluster_objects(&run, |c| c.x0, x_tol, false)
        };
        for mut line in lines {
            if upright {
                line.sort_by(|a, b| cmp_f(a.x0, b.x0));
            } else {
                line.sort_by(|a, b| cmp_f(a.top, b.top).then(cmp_f(a.bottom, b.bottom)));
            }
            let mut current: Vec<Char> = Vec::new();
            for c in line {
                if py_isspace(&c.text) {
                    if !current.is_empty() {
                        let w = std::mem::take(&mut current);
                        out.push((merge(&w), w));
                    }
                } else if !current.is_empty() && begins_new_word(current.last().unwrap(), &c, upright, x_tol, y_tol) {
                    let w = std::mem::replace(&mut current, vec![c]);
                    out.push((merge(&w), w));
                } else {
                    current.push(c);
                }
            }
            if !current.is_empty() {
                out.push((merge(&current), current));
            }
        }
    }
    out
}

/// `page.extract_words(x_tolerance=x, y_tolerance=y)`.
pub fn extract_words(chars: &[Char], x_tol: f64, y_tol: f64) -> Vec<Word> {
    extract_tuples(chars, x_tol, y_tol).into_iter().map(|(w, _)| w).collect()
}

/// `page.extract_text()` with its defaults (no layout).
pub fn extract_text(chars: &[Char]) -> String {
    if chars.is_empty() {
        return String::new();
    }
    let tuples = extract_tuples(chars, 3.0, 3.0);
    let lines = cluster_objects(&tuples, |t| t.0.top, 3.0, true);
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut line_len = 0usize;
        for (_, wchars) in line {
            if line_len > 0 {
                out.push(' ');
                line_len += 1;
            }
            for c in wchars {
                let letters = ligature(&c.text);
                for ch in letters.chars() {
                    out.push(ch);
                    line_len += 1;
                }
            }
        }
    }
    out
}
