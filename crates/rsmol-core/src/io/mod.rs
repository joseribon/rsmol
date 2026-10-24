mod builder;
pub mod cif;
pub mod mmcif;
pub mod pdb;

use std::fmt;
use std::path::Path;

use crate::Structure;

/// Reads a structure file, picking the format from the extension.
pub fn read(path: impl AsRef<Path>) -> Result<Structure, ParseError> {
    let path = path.as_ref();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdb" | "ent" => pdb::read(path),
        "cif" | "mmcif" => mmcif::read(path),
        _ => Err(ParseError::msg(format!("{}: unknown structure format", path.display()))),
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
