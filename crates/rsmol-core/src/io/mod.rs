pub mod pdb;

use std::fmt;
use std::path::Path;

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

    pub(crate) fn io(path: &Path, err: std::io::Error) -> Self {
        ParseError { line: None, message: format!("{}: {err}", path.display()) }
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
