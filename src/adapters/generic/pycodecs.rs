//! Python's codecs, as `ofxparse` uses them: `codecs.lookup(name)` and strict decoding with Python's error messages.

use super::ofx_tables::{CODECS, SINGLE_BYTE};

#[derive(Clone, Copy, Debug)]
pub enum Codec {
    Ascii,
    Latin1,
    Utf8,
    Charmap(&'static [u32; 256]),
}

/// `encodings.normalize_encoding`: runs of other characters than ASCII letters, digits and "." become one "_",
/// none at either end.
fn normalize_encoding(name: &str) -> String {
    let mut out = String::new();
    let mut punct = false;
    for c in name.chars() {
        if crate::py::is_alnum_char(c) || c == '.' {
            if punct && !out.is_empty() {
                out.push('_');
            }
            if c.is_ascii() {
                out.push(c);
            }
            punct = false;
        } else {
            punct = true;
        }
    }
    out
}

/// `codecs.lookup(name)`, or `LookupError`'s message.
pub fn lookup(name: &str) -> Result<Codec, String> {
    let unknown = || format!("unknown encoding: {name}");
    if !name.is_ascii() {
        return Err(unknown());
    }
    let key = normalize_encoding(&name.to_ascii_lowercase());
    let found = |k: &str| CODECS.iter().find(|(n, _)| *n == k).map(|(_, c)| *c);
    let codec = found(&key).or_else(|| found(&key.replace('.', "_"))).ok_or_else(unknown)?;
    Ok(match codec {
        "Ascii" => Codec::Ascii,
        "Latin1" => Codec::Latin1,
        "Utf8" => Codec::Utf8,
        other => Codec::Charmap(&SINGLE_BYTE.iter().find(|(n, _)| *n == other).ok_or_else(unknown)?.1),
    })
}

fn position(start: usize, len: usize) -> String {
    if len == 1 {
        format!("in position {start}")
    } else {
        format!("in position {}-{}", start, start + len - 1)
    }
}

/// Decode strictly. `last` is false for a stream reader: a UTF-8 sequence cut at the end is kept back (and lost).
pub fn decode(data: &[u8], codec: Codec, last: bool) -> Result<String, String> {
    match codec {
        Codec::Latin1 => Ok(data.iter().map(|&b| b as char).collect()),
        Codec::Ascii => match data.iter().position(|b| *b >= 0x80) {
            Some(p) => Err(format!("'ascii' codec can't decode byte 0x{:02x} in position {p}: ordinal not in range(128)", data[p])),
            None => Ok(data.iter().map(|&b| b as char).collect()),
        },
        Codec::Charmap(table) => {
            let mut out = String::with_capacity(data.len());
            for (p, &b) in data.iter().enumerate() {
                match char::from_u32(table[b as usize]) {
                    Some(c) if table[b as usize] != 0xFFFF_FFFF => out.push(c),
                    _ => return Err(format!("'charmap' codec can't decode byte 0x{b:02x} in position {p}: character maps to <undefined>")),
                }
            }
            Ok(out)
        }
        Codec::Utf8 => match std::str::from_utf8(data) {
            Ok(s) => Ok(s.to_string()),
            Err(e) => {
                let start = e.valid_up_to();
                match e.error_len() {
                    Some(n) => {
                        let first = data[start];
                        let reason = if (0x80..0xc2).contains(&first) || first >= 0xf5 { "invalid start byte" } else { "invalid continuation byte" };
                        let what = if n == 1 { format!("byte 0x{first:02x}") } else { "bytes".to_string() };
                        Err(format!("'utf-8' codec can't decode {what} {}: {reason}", position(start, n)))
                    }
                    None if !last => Ok(String::from_utf8_lossy(&data[..start]).into_owned()),
                    None => {
                        let n = data.len() - start;
                        let what = if n == 1 { format!("byte 0x{:02x}", data[start]) } else { "bytes".to_string() };
                        Err(format!("'utf-8' codec can't decode {what} {}: unexpected end of data", position(start, n)))
                    }
                }
            }
        },
    }
}

/// `bytes.decode("ascii", "replace")`.
pub fn ascii_replace(data: &[u8]) -> String {
    data.iter().map(|&b| if b < 0x80 { b as char } else { '\u{fffd}' }).collect()
}

/// `bytes.decode("utf-8", "ignore")` (with "utf-8-sig", a leading BOM is dropped too).
pub fn utf8_ignore(data: &[u8], sig: bool) -> String {
    let data = if sig { data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data) } else { data };
    data.utf8_chunks().map(|c| c.valid()).collect()
}
