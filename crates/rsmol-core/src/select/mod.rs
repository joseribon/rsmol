//! PyMOL-compatible selection language: parsing and evaluation.

pub mod ast;
mod atomset;
mod eval;
mod parser;

use std::ops::Range;

pub use ast::Expr;
pub use atomset::AtomSet;
pub use eval::{EvalError, Evaluator};
pub use parser::{SelectionError, parse};

use crate::Structure;

/// Parses and evaluates `text` against state 0 of `s`, with no named selections.
pub fn select(s: &Structure, text: &str) -> Result<AtomSet, SelectionError> {
    let expr = parse(text)?;
    Evaluator::new(s, 0).eval(&expr).map_err(|e| e.into_selection_error(text))
}

impl EvalError {
    /// Converts to the parser's error type; errors without a span cover the whole input.
    pub fn into_selection_error(self, text: &str) -> SelectionError {
        let span: Range<usize> = self.span.unwrap_or(0..text.len());
        SelectionError { span, message: self.message }
    }
}
