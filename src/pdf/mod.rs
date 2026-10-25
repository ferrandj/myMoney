//! Reading PDF pages the way pdfplumber (over pdfminer.six) reads them: the same characters, the same
//! positions, the same words and the same text, so that every adapter sees exactly what the Python version saw.

pub mod crypt;
pub mod document;
pub mod font;
pub mod interp;
pub mod lexer;
pub mod objects;
pub mod parser;
pub mod tables;
pub mod text;

pub use interp::Char;
pub use text::Word;

#[derive(Debug)]
pub enum Error {
    /// The file is protected by a password (pdfminer: PDFPasswordIncorrect).
    Password,
    Invalid(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // pdfplumber wraps pdfminer's PDFPasswordIncorrect, which has no message
            Error::Password => write!(f, ""),
            Error::Invalid(s) => write!(f, "{s}"),
        }
    }
}

/// The characters of every page.
pub fn chars(data: &[u8]) -> Result<Vec<Vec<Char>>, Error> {
    let doc = document::Document::open(data.to_vec()).map_err(|e| match e {
        document::PdfError::Encrypted => Error::Password,
        document::PdfError::Crypt(m) => Error::Invalid(m),
        other => Error::Invalid(other.to_string()),
    })?;
    interp::page_chars(&doc).map_err(|e| Error::Invalid(e.0))
}

/// `page.extract_words(x_tolerance=x, y_tolerance=2)` for every page.
pub fn extract_words(data: &[u8], x_tolerance: f64) -> Result<Vec<Vec<Word>>, Error> {
    Ok(chars(data)?.iter().map(|c| text::extract_words(c, x_tolerance, 2.0)).collect())
}

/// `"\n".join(page.extract_text() or "" for page in pdf.pages)`.
pub fn extract_text(data: &[u8]) -> Result<String, Error> {
    Ok(chars(data)?.iter().map(|c| text::extract_text(c)).collect::<Vec<_>>().join("\n"))
}
