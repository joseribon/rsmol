//! The command bar: a small subset of PyMOL's command language.
//!
//!   select <selection>           store as "sele" and highlight
//!   select <name>, <selection>   store under <name> and highlight
//!   zoom [<selection>]           frame the selection (or everything)
//!   deselect                     clear "sele" and the highlight
//!   <selection>                  same as `select <selection>`
//!
//! Every command can be dry-run, which evaluates without side effects for live feedback.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use rsmol_core::Structure;
use rsmol_core::select::{AtomSet, Evaluator, SelectionError, parse};

use crate::viewport::Msg;

/// State the command bar works on: the loaded structure and its named selections.
#[derive(Default)]
pub struct Session {
    pub structure: Option<Arc<Structure>>,
    pub named: HashMap<String, AtomSet>,
}

/// What a command did (or, for a dry run, would do).
#[derive(Debug)]
pub struct Outcome {
    pub message: String,
    pub canonical: Option<String>,
    pub atoms: Option<usize>,
    /// Viewport updates to apply; empty for dry runs.
    pub effects: Vec<Msg>,
}

enum Command<'a> {
    Select { name: &'a str, expr: &'a str, offset: usize },
    Zoom { expr: Option<(&'a str, usize)> },
    Deselect,
}

const DEFAULT_SELECTION: &str = "sele";

fn error(span: Range<usize>, message: impl Into<String>) -> SelectionError {
    SelectionError { span, message: message.into() }
}

/// `s` without surrounding whitespace, and its new offset given that `s` starts at `offset`.
fn trimmed(s: &str, offset: usize) -> (&str, usize) {
    (s.trim(), offset + s.len() - s.trim_start().len())
}

/// Splits off the command word. Byte offsets in the result index into `text`.
fn parse_command(text: &str) -> Result<Command<'_>, SelectionError> {
    let start = text.len() - text.trim_start().len();
    let rest = &text[start..];
    let word_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let word = &rest[..word_len];
    // The argument string and its offset in `text`.
    let args_offset = start + word_len;
    let args = &text[args_offset..];

    match word.to_ascii_lowercase().as_str() {
        "select" => {
            let (args, offset) = trimmed(args, args_offset);
            if args.is_empty() {
                return Err(error(text.len()..text.len(), "select needs a selection"));
            }
            match args.split_once(',') {
                Some((name, expr)) => {
                    let name = name.trim();
                    let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                    if !valid {
                        let span = offset..offset + args.find(',').unwrap();
                        return Err(error(span, "selection names use letters, digits and _"));
                    }
                    let expr_offset = offset + args.find(',').unwrap() + 1;
                    let (expr, expr_offset) = trimmed(expr, expr_offset);
                    if expr.is_empty() {
                        return Err(error(text.len()..text.len(), "expected a selection after ','"));
                    }
                    Ok(Command::Select { name, expr, offset: expr_offset })
                }
                None => Ok(Command::Select { name: DEFAULT_SELECTION, expr: args, offset }),
            }
        }
        "zoom" => {
            let (args, offset) = trimmed(args, args_offset);
            Ok(Command::Zoom { expr: (!args.is_empty()).then_some((args, offset)) })
        }
        "deselect" => Ok(Command::Deselect),
        _ => {
            let (expr, offset) = trimmed(text, 0);
            Ok(Command::Select { name: DEFAULT_SELECTION, expr, offset })
        }
    }
}

impl Session {
    pub fn run(&mut self, text: &str, dry_run: bool) -> Result<Outcome, SelectionError> {
        let command = parse_command(text)?;
        if let Command::Deselect = command {
            let mut effects = Vec::new();
            if !dry_run {
                self.named.remove(DEFAULT_SELECTION);
                effects.push(Msg::Highlight(Vec::new()));
            }
            return Ok(Outcome { message: "clear the selection".into(), canonical: None, atoms: None, effects });
        }

        let structure = self.structure.clone().ok_or_else(|| error(0..text.len(), "no structure loaded"))?;
        let evaluate = |expr: &str, offset: usize| -> Result<(AtomSet, String), SelectionError> {
            let shift = |e: SelectionError| error(e.span.start + offset..e.span.end + offset, e.message);
            let parsed = parse(expr).map_err(shift)?;
            let set = Evaluator::new(&structure, 0)
                .with_named(&self.named)
                .eval(&parsed)
                .map_err(|e| shift(e.into_selection_error(expr)))?;
            Ok((set, parsed.to_string()))
        };
        let residues = |set: &AtomSet| set.iter().map(|i| structure.atoms.residue[i]).collect::<BTreeSet<_>>().len();

        match command {
            Command::Select { name, expr, offset } => {
                let (set, canonical) = evaluate(expr, offset)?;
                let (count, res) = (set.count(), residues(&set));
                let mut effects = Vec::new();
                if !dry_run {
                    effects.push(Msg::Highlight(set.iter().collect()));
                    self.named.insert(name.to_owned(), set);
                }
                Ok(Outcome {
                    message: format!("{name}: {count} atoms in {res} residues"),
                    canonical: Some(canonical),
                    atoms: Some(count),
                    effects,
                })
            }
            Command::Zoom { expr } => {
                let (target, canonical, count) = match expr {
                    Some((expr, offset)) => {
                        let (set, canonical) = evaluate(expr, offset)?;
                        if set.is_empty() {
                            return Err(error(offset..offset + expr.len(), "selection is empty"));
                        }
                        let count = set.count();
                        (Some(set.iter().collect()), Some(canonical), Some(count))
                    }
                    None => (None, None, Some(structure.atom_count())),
                };
                let message = match count {
                    Some(n) if target.is_some() => format!("zoom to {n} atoms"),
                    _ => "zoom to everything".to_owned(),
                };
                let effects = if dry_run { Vec::new() } else { vec![Msg::Focus(target)] };
                Ok(Outcome { message, canonical, atoms: count, effects })
            }
            Command::Deselect => unreachable!("handled above"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let text = "\
ATOM      1  N   ALA A   1      -1.200   0.500   0.000  1.00  0.00           N
ATOM      2  CA  ALA A   1       0.000   0.000   0.000  1.00  0.00           C
HETATM    3 ZN    ZN A 101       5.000   0.000   0.000  1.00  0.00          ZN
";
        let s = rsmol_core::io::pdb::parse(text, "t").unwrap();
        Session { structure: Some(Arc::new(s)), named: HashMap::new() }
    }

    #[test]
    fn select_stores_named_selections() {
        let mut s = session();
        let out = s.run("select metal_site, metals", false).unwrap();
        assert_eq!(out.message, "metal_site: 1 atoms in 1 residues");
        assert_eq!(s.named["metal_site"].iter().collect::<Vec<_>>(), [2]);
        // A bare selection goes to "sele" and can reference earlier names.
        s.run("metal_site or name CA", false).unwrap();
        assert_eq!(s.named["sele"].count(), 2);
    }

    #[test]
    fn sele_is_a_name_not_a_command() {
        let mut s = session();
        s.run("name CA", false).unwrap();
        s.run("sele or metals", false).unwrap();
        assert_eq!(s.named["sele"].count(), 2);
    }

    #[test]
    fn dry_runs_have_no_effects() {
        let mut s = session();
        let out = s.run("name CA", true).unwrap();
        assert!(out.effects.is_empty());
        assert!(s.named.is_empty());
    }

    #[test]
    fn error_spans_point_into_the_full_command() {
        let mut s = session();
        let text = "select site, name CA and bogus";
        let err = s.run(text, true).unwrap_err();
        assert_eq!(&text[err.span.clone()], "bogus");
        let err = s.run("select 1x, all", true).unwrap_err();
        assert_eq!(err.message, "selection names use letters, digits and _");
        let err = s.run("zoom resn XYZ", true).unwrap_err();
        assert_eq!(err.message, "selection is empty");
    }
}
