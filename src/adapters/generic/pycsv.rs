//! Python's `csv` module, as far as the CSV adapters use it: `csv.Sniffer().sniff(sample, delimiters)` (only the
//! delimiter it finds matters) and `csv.reader(io.StringIO(text), delimiter=d)` with the "excel" dialect otherwise
//! (CPython 3.13 `_csv.c` state machine, non-strict).

use std::collections::HashMap;

use fancy_regex::Regex as FancyRegex;

pub const FIELD_LIMIT: usize = 131072;

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    StartRecord,
    StartField,
    InField,
    InQuotedField,
    QuoteInQuotedField,
    EatCrnl,
}

/// `list(csv.reader(io.StringIO(text), delimiter=delimiter))`, or the error message of `csv.Error`.
pub fn reader(text: &str, delimiter: char) -> Result<Vec<Vec<String>>, String> {
    const QUOTE: char = '"';
    let mut rows: Vec<Vec<String>> = vec![];
    // io.StringIO yields lines ending with "\n" (kept); no other character ends a line.
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    if text.is_empty() {
        lines.clear();
    }
    let mut it = lines.into_iter();
    loop {
        // one record
        let mut fields: Vec<String> = vec![];
        let mut field = String::new();
        let mut field_len = 0usize;
        let mut state = State::StartRecord;
        let save = |fields: &mut Vec<String>, field: &mut String, field_len: &mut usize| {
            fields.push(std::mem::take(field));
            *field_len = 0;
        };
        loop {
            let Some(line) = it.next() else {
                if field_len != 0 || state == State::InQuotedField {
                    save(&mut fields, &mut field, &mut field_len);
                    rows.push(fields);
                }
                return Ok(rows);
            };
            for c in line.chars().map(Some).chain(std::iter::once(None)) {
                // None is the end of the line (EOL)
                let nl = matches!(c, Some('\n') | Some('\r'));
                let add = |field: &mut String, field_len: &mut usize, ch: char| -> Result<(), String> {
                    if *field_len >= FIELD_LIMIT {
                        return Err(format!("field larger than field limit ({FIELD_LIMIT})"));
                    }
                    field.push(ch);
                    *field_len += 1;
                    Ok(())
                };
                let mut st = state;
                if st == State::StartRecord {
                    if c.is_none() {
                        state = State::StartRecord;
                        continue;
                    } else if nl {
                        state = State::EatCrnl;
                        continue;
                    }
                    st = State::StartField;
                }
                match st {
                    State::StartField => {
                        if nl || c.is_none() {
                            save(&mut fields, &mut field, &mut field_len);
                            state = if c.is_none() { State::StartRecord } else { State::EatCrnl };
                        } else if c == Some(QUOTE) {
                            state = State::InQuotedField;
                        } else if c == Some(delimiter) {
                            save(&mut fields, &mut field, &mut field_len);
                            state = State::StartField;
                        } else {
                            add(&mut field, &mut field_len, c.unwrap())?;
                            state = State::InField;
                        }
                    }
                    State::InField => {
                        if nl || c.is_none() {
                            save(&mut fields, &mut field, &mut field_len);
                            state = if c.is_none() { State::StartRecord } else { State::EatCrnl };
                        } else if c == Some(delimiter) {
                            save(&mut fields, &mut field, &mut field_len);
                            state = State::StartField;
                        } else {
                            add(&mut field, &mut field_len, c.unwrap())?;
                            state = State::InField;
                        }
                    }
                    State::InQuotedField => {
                        if c.is_none() {
                            state = State::InQuotedField;
                        } else if c == Some(QUOTE) {
                            state = State::QuoteInQuotedField;
                        } else {
                            add(&mut field, &mut field_len, c.unwrap())?;
                            state = State::InQuotedField;
                        }
                    }
                    State::QuoteInQuotedField => {
                        if c == Some(QUOTE) {
                            add(&mut field, &mut field_len, QUOTE)?;
                            state = State::InQuotedField;
                        } else if c == Some(delimiter) {
                            save(&mut fields, &mut field, &mut field_len);
                            state = State::StartField;
                        } else if nl || c.is_none() {
                            save(&mut fields, &mut field, &mut field_len);
                            state = if c.is_none() { State::StartRecord } else { State::EatCrnl };
                        } else {
                            add(&mut field, &mut field_len, c.unwrap())?;
                            state = State::InField;
                        }
                    }
                    State::EatCrnl => {
                        if nl {
                            state = State::EatCrnl;
                        } else if c.is_none() {
                            state = State::StartRecord;
                        } else {
                            return Err("new-line character seen in unquoted field - do you need to open the file with newline=''?".into());
                        }
                    }
                    State::StartRecord => unreachable!(),
                }
            }
            if state == State::StartRecord {
                break;
            }
        }
        rows.push(fields);
    }
}

/// `csv.Sniffer().sniff(sample, delimiters).delimiter`, or None where it raises "Could not determine delimiter".
pub fn sniff_delimiter(sample: &str, delimiters: &str) -> Option<char> {
    let sample = sample.replace("\r\n", "\n").replace('\r', "\n");
    if let Some(d) = guess_quote_and_delimiter(&sample, delimiters) {
        return Some(d);
    }
    guess_delimiter(&sample, delimiters)
}

/// Python's `max(d, key=d.get)`: the first key (in insertion order) with the highest count.
fn first_max(order: &[char], counts: &HashMap<char, usize>) -> Option<char> {
    let mut best: Option<(char, usize)> = None;
    for k in order {
        let v = counts[k];
        if best.map(|(_, b)| v > b).unwrap_or(true) {
            best = Some((*k, v));
        }
    }
    best.map(|(k, _)| k)
}

fn guess_quote_and_delimiter(data: &str, delimiters: &str) -> Option<char> {
    let body = r#"(?:(?P=quote){2}|(?!(?P=quote)).)*+"#;
    // `[^\w\n"']` with Python's `\w`: what `str.isalnum()` accepts (letters and numbers of any script) and "_".
    let not_word = r#"[^\p{L}\p{N}_\n"']"#;
    let patterns = [
        format!(r#"(?P<delim>{not_word})(?P<space> ?)(?P<quote>["']){body}(?P=quote)(?P=delim)"#),
        format!(r#"(?:^|\n)(?P<quote>["']){body}(?P=quote)(?P<delim>{not_word})(?P<space> ?)"#),
        format!(r#"(?P<delim>{not_word})(?P<space> ?)(?P<quote>["']){body}(?P=quote)(?:$|\n)"#),
        format!(r#"(?:^|\n)(?P<quote>["']){body}(?P=quote)(?:$|\n)"#),
    ];
    for (i, p) in patterns.iter().enumerate() {
        let re = FancyRegex::new(&format!("(?sm){p}")).expect("sniffer regex");
        let mut found: Vec<(Option<String>, Option<String>)> = vec![];
        let mut at = 0;
        while at <= data.len() {
            let Ok(Some(c)) = re.captures_from_pos(data, at) else { break };
            let whole = c.get(0).unwrap();
            found.push((c.name("quote").map(|m| m.as_str().to_string()), if i < 3 { Some(c.name("delim").map(|m| m.as_str().to_string()).unwrap_or_default()) } else { None }));
            at = if whole.end() > whole.start() {
                whole.end()
            } else {
                // an empty match: step over one character
                whole.end() + data[whole.end()..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
            };
        }
        if found.is_empty() {
            continue;
        }
        let mut order: Vec<char> = vec![];
        let mut delims: HashMap<char, usize> = HashMap::new();
        for (_, delim) in &found {
            let Some(d) = delim else { continue };
            let Some(ch) = d.chars().next() else { continue };
            if delimiters.contains(ch) {
                if !delims.contains_key(&ch) {
                    order.push(ch);
                }
                *delims.entry(ch).or_insert(0) += 1;
            }
        }
        let delim = first_max(&order, &delims)?;
        return if delim == '\n' { None } else { Some(delim) };
    }
    None
}

fn guess_delimiter(data: &str, delimiters: &str) -> Option<char> {
    let data: Vec<&str> = data.split('\n').filter(|l| !l.is_empty()).collect();
    let ascii: Vec<char> = (0u8..127).map(|c| c as char).collect();
    let chunk_length = data.len().min(10);
    let mut iteration = 0usize;
    // char -> [(frequency, rows)], in insertion order
    let mut char_frequency: Vec<Vec<(usize, usize)>> = vec![vec![]; 127];
    let mut seen = [false; 127];
    let mut modes: Vec<Option<(usize, i64)>> = vec![None; 127];
    let mut mode_order: Vec<usize> = vec![];
    let mut delims_order: Vec<usize> = vec![];
    let mut delims: HashMap<usize, (usize, i64)> = HashMap::new();
    let (mut start, mut end) = (0usize, chunk_length);
    let preferred = [',', '\t', ';', ' ', ':'];
    while start < data.len() {
        iteration += 1;
        for line in &data[start.min(data.len())..end.min(data.len())] {
            let mut counts = [0usize; 127];
            for ch in line.chars() {
                if (ch as u32) < 127 {
                    counts[ch as usize] += 1;
                }
            }
            for (c, &freq) in counts.iter().enumerate() {
                seen[c] = true;
                let meta = &mut char_frequency[c];
                match meta.iter_mut().find(|(f, _)| *f == freq) {
                    Some(e) => e.1 += 1,
                    None => meta.push((freq, 1)),
                }
            }
        }
        for c in 0..127 {
            if !seen[c] {
                continue;
            }
            let items = &char_frequency[c];
            if items.len() == 1 && items[0].0 == 0 {
                continue;
            }
            let mode = if items.len() > 1 {
                let mut best = 0;
                for (i, it) in items.iter().enumerate() {
                    if it.1 > items[best].1 {
                        best = i;
                    }
                }
                let others: usize = items.iter().enumerate().filter(|(i, _)| *i != best).map(|(_, x)| x.1).sum();
                (items[best].0, items[best].1 as i64 - others as i64)
            } else {
                (items[0].0, items[0].1 as i64)
            };
            if modes[c].is_none() {
                mode_order.push(c);
            }
            modes[c] = Some(mode);
        }
        let total = (chunk_length * iteration).min(data.len()) as f64;
        let mut consistency = 1.0f64;
        let threshold = 0.9f64;
        while delims.is_empty() && consistency >= threshold {
            for &k in &mode_order {
                let v = modes[k].unwrap();
                if v.0 > 0 && v.1 > 0 && (v.1 as f64 / total) >= consistency && delimiters.contains(ascii[k]) {
                    if !delims.contains_key(&k) {
                        delims_order.push(k);
                    }
                    delims.insert(k, v);
                }
            }
            consistency -= 0.01;
        }
        if delims.len() == 1 {
            return Some(ascii[delims_order[0]]);
        }
        start = end;
        end += chunk_length;
        if chunk_length == 0 {
            break;
        }
    }
    if delims.is_empty() {
        return None;
    }
    if delims.len() > 1 {
        for d in preferred {
            if delims.contains_key(&(d as usize)) {
                return Some(d);
            }
        }
    }
    // the character that dominates: the largest (mode, count), then the largest character
    let mut items: Vec<((usize, i64), usize)> = delims.iter().map(|(k, v)| (*v, *k)).collect();
    items.sort();
    items.last().map(|(_, k)| ascii[*k])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_edge_cases() {
        assert!(reader("a;b\rc;d\n", ';').is_err());
        assert_eq!(reader("a;\"b\nc\";d\ne;f", ';').unwrap(), vec![vec!["a", "b\nc", "d"], vec!["e", "f"]]);
        assert_eq!(reader("\"ab\"c;d\n", ';').unwrap(), vec![vec!["abc", "d"]]);
        assert_eq!(reader("\"abc", ';').unwrap(), vec![vec!["abc"]]);
        let empty: Vec<String> = vec![];
        assert_eq!(reader("a;b\r\n\r\nc", ';').unwrap(), vec![vec!["a".to_string(), "b".to_string()], empty, vec!["c".to_string()]]);
    }
}
