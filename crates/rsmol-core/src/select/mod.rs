//! PyMOL-compatible selection language.
//!
//! Only parsing exists so far; evaluation lands once bonds and spatial indexing are in the
//! data model.

pub mod ast;
mod parser;

pub use ast::Expr;
pub use parser::{SelectionError, parse};
