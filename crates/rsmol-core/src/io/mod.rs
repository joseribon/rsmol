mod builder;
pub mod cif;
pub mod mmcif;
pub mod pdb;
pub mod write;

use std::fmt;
use std::path::Path;

use crate::Structure;

/// Reads a structure file, picking the format from the extension. Gzipped files
/// (`.cif.gz`, `.pdb.gz`, …) are decompressed transparently.
pub fn read(path: impl AsRef<Path>) -> Result<Structure, ParseError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| ParseError::io(path, e))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    parse_bytes(&bytes, name).map_err(|e| match e.line {
        Some(_) => e,
        None => ParseError::msg(format!("{}: {}", path.display(), e.message)),
    })
}

/// Parses file contents, choosing the format from `file_name`'s extensions.
pub fn parse_bytes(bytes: &[u8], file_name: &str) -> Result<Structure, ParseError> {
    let lower = file_name.to_ascii_lowercase();
    let (inner, gzipped) = match lower.strip_suffix(".gz") {
        Some(inner) => (inner, true),
        None => (lower.as_str(), false),
    };
    let (stem, ext) = inner.rsplit_once('.').unwrap_or((inner, ""));
    // Keep the original capitalization of the name (e.g. "1HSG").
    let name = &file_name[..stem.len()];

    let decompressed;
    let data = if gzipped {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::MultiGzDecoder::new(bytes), &mut out)
            .map_err(|e| ParseError::msg(format!("not a valid gzip file: {e}")))?;
        decompressed = out;
        decompressed.as_slice()
    } else {
        bytes
    };
    let text = std::str::from_utf8(data).map_err(|e| ParseError::msg(format!("not UTF-8 text: {e}")))?;
    match ext {
        "pdb" | "ent" => pdb::parse(text, name),
        "cif" | "mmcif" => mmcif::parse(text, name),
        _ => Err(ParseError::msg(format!("unknown structure format '.{ext}'"))),
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

#[derive(Debug)]
pub struct ParseError {
    /// 1-based line number, when the error is tied to a line.
    pub line: Option<usize>,
    pub message: String,
}

impl ParseError {
    pub(crate) fn at(line: usize, message: impl Into<String>) -> Self {
        ParseError { line: Some(line), message: message.into() }
    }

    pub(crate) fn msg(message: impl Into<String>) -> Self {
        ParseError { line: None, message: message.into() }
    }

    pub(crate) fn io(path: &Path, err: std::io::Error) -> Self {
        ParseError::msg(format!("{}: {err}", path.display()))
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ParseError {}
