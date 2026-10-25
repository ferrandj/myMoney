//! A tiny PDF writer for the tests: Helvetica 9 pt text placed on A4-like pages (595 x 842 pt), the way the Python
//! tests draw their synthetic documents with fpdf2. Words sit at exact coordinates, so the adapters read the same
//! geometry as in real life.

/// Helvetica advance widths (1/1000 em) by Windows-1252 byte.
const WIDTHS: [u16; 256] = [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,278,278,355,556,556,889,667,191,333,333,389,584,278,333,278,278,556,556,556,556,556,556,556,556,556,556,278,278,584,584,584,556,1015,667,667,722,722,667,611,778,722,278,500,667,556,833,722,778,667,778,722,667,611,722,667,944,667,667,611,278,278,278,469,556,333,556,556,500,556,556,278,556,556,222,222,500,222,833,556,556,556,556,333,500,278,556,500,722,500,500,500,334,260,334,584,0,556,0,222,556,333,1000,556,556,333,1000,667,333,1000,0,611,0,0,222,222,333,333,350,556,1000,333,1000,500,333,944,0,500,667,278,333,556,556,556,556,260,556,333,737,370,556,584,333,737,333,400,584,333,333,333,556,537,278,333,333,365,556,834,834,834,611,667,667,667,667,667,667,1000,722,667,667,667,667,278,278,278,278,722,722,778,778,778,778,778,584,778,722,722,722,722,667,667,611,556,556,556,556,556,556,889,500,556,556,556,556,278,278,278,278,556,556,556,556,556,556,556,584,611,556,556,556,556,500,556,500];

/// The Windows-1252 byte of a character (what fpdf2 writes with core_fonts_encoding = "cp1252").
pub fn cp1252(c: char) -> u8 {
    match c {
        '€' => 0x80, '‚' => 0x82, 'ƒ' => 0x83, '„' => 0x84, '…' => 0x85, '†' => 0x86, '‡' => 0x87, 'ˆ' => 0x88,
        '‰' => 0x89, 'Š' => 0x8a, '‹' => 0x8b, 'Œ' => 0x8c, 'Ž' => 0x8e, '‘' => 0x91, '’' => 0x92, '“' => 0x93,
        '”' => 0x94, '•' => 0x95, '–' => 0x96, '—' => 0x97, '˜' => 0x98, '™' => 0x99, 'š' => 0x9a, '›' => 0x9b,
        'œ' => 0x9c, 'ž' => 0x9e, 'Ÿ' => 0x9f,
        c if (c as u32) < 256 => c as u32 as u8,
        _ => b'?',
    }
}

pub struct Pdf {
    title: String,
    pages: Vec<String>,
}

impl Pdf {
    pub fn new(title: &str) -> Pdf {
        Pdf { title: title.to_string(), pages: vec![String::new()] }
    }

    pub fn add_page(&mut self) {
        self.pages.push(String::new());
    }

    /// Width in points of a text at 9 pt.
    pub fn width(&self, text: &str) -> f64 {
        text.chars().map(|c| WIDTHS[cp1252(c) as usize] as f64).sum::<f64>() * 9.0 / 1000.0
    }

    /// The baseline of the text starts at (x, y), measured from the top-left corner of the page.
    pub fn text(&mut self, x: f64, y: f64, text: &str) {
        let mut escaped = String::new();
        for c in text.chars() {
            let b = cp1252(c);
            match b {
                b'(' | b')' | b'\\' => {
                    escaped.push('\\');
                    escaped.push(b as char);
                }
                0x20..=0x7e => escaped.push(b as char),
                _ => escaped.push_str(&format!("\\{:03o}", b)),
            }
        }
        let page = self.pages.last_mut().unwrap();
        page.push_str(&format!("BT /F1 9 Tf {:.2} {:.2} Td ({}) Tj ET\n", x, 842.0 - y, escaped));
    }

    /// Text whose right edge is at `right`.
    pub fn right(&mut self, right: f64, y: f64, text: &str) {
        let x = right - self.width(text);
        self.text(x, y, text);
    }

    pub fn bytes(&self) -> Vec<u8> {
        let n = self.pages.len();
        // objects: 1 pages, 2 catalog, 3 font, 4 info, then (page, contents) pairs from 5
        let mut objects: Vec<Vec<u8>> = vec![];
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 5 + 2 * i)).collect();
        objects.push(format!("<< /Type /Pages /Count {n} /Kids [{}] >>", kids.join(" ")).into_bytes());
        objects.push(b"<< /Type /Catalog /Pages 1 0 R >>".to_vec());
        objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec());
        let title = self.title.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)");
        objects.push(format!("<< /Title ({title}) /CreationDate (D:20261001000000Z) >>").into_bytes());
        for (i, content) in self.pages.iter().enumerate() {
            objects.push(format!("<< /Type /Page /Parent 1 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 6 + 2 * i).into_bytes());
            let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
            stream.extend(content.bytes());
            stream.extend(b"endstream");
            objects.push(stream);
        }
        let mut out = b"%PDF-1.3\n".to_vec();
        let mut offsets = vec![];
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).bytes());
            out.extend(obj);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 2 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
        out
    }
}
