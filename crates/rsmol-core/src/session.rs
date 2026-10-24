//! A session and its command language: a subset of PyMOL's, shared by every front end.
//!
//!   select [<name>,] <selection>     store (default "sele") and highlight
//!   deselect                         clear "sele" and the highlight
//!   zoom [<selection>]               frame the selection, or everything
//!   show <rep> [, <selection>]       add a representation (spheres, sticks, cartoon)
//!   hide <rep|everything> [, <sel>]  remove representations
//!   as <rep> [, <selection>]         show only that representation
//!   color <color> [, <selection>]    a name or #rrggbb, or a scheme: bychain, byelement,
//!                                    byss, bfactor, default
//!   <selection>                      same as `select <selection>`
//!
//! Commands separated by `;` run in order. Every line can be dry-run, which evaluates without
//! side effects, for live feedback while typing.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use crate::color::{self, Rgba};
use crate::display::{self, Display, Reps};
use crate::select::{AtomSet, Evaluator, SelectionError, parse};
use crate::Structure;

const DEFAULT_SELECTION: &str = "sele";

/// One-click looks, as command lines, so front ends can show users the commands they run.
pub const PRESETS: &[(&str, &str)] = &[
    ("Cartoon + ligands", "hide everything; show cartoon; show sticks, not polymer and not solvent"),
    ("Cartoon", "hide everything; show cartoon"),
    ("Sticks", "hide everything; show sticks, not solvent"),
    ("Spheres", "hide everything; show spheres, not solvent"),
];

#[derive(Default)]
pub struct Session {
    pub structure: Option<Arc<Structure>>,
    pub display: Option<Display>,
    pub named: HashMap<String, AtomSet>,
}

/// What a front end must update after a command.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Mark these atoms as selected; empty clears the markers.
    Highlight(Vec<usize>),
    /// Frame these atoms, or everything for `None`.
    Focus(Option<Vec<usize>>),
    /// `Session::display` changed.
    Display,
}

/// What a command did (or, for a dry run, would do).
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub message: String,
    /// Canonical form of the selection argument, if there was one.
    pub canonical: Option<String>,
    pub atoms: Option<usize>,
    /// Empty for dry runs.
    pub effects: Vec<Effect>,
}

enum Color {
    Fixed(Rgba),
    ByChain,
    ByElement,
    BySs,
    BFactor,
    Default,
}

enum Command<'a> {
    Select { name: &'a str, sel: Arg<'a> },
    Deselect,
    Zoom { sel: Option<Arg<'a>> },
    Show { reps: Reps, sel: Option<Arg<'a>> },
    Hide { reps: Reps, sel: Option<Arg<'a>> },
    As { reps: Reps, sel: Option<Arg<'a>> },
    Color { color: Color, sel: Option<Arg<'a>> },
}

/// A selection argument and its byte offset in the full command line.
#[derive(Clone, Copy)]
struct Arg<'a> {
    text: &'a str,
    offset: usize,
}

fn error(span: Range<usize>, message: impl Into<String>) -> SelectionError {
    SelectionError { span, message: message.into() }
}

/// `s` without surrounding whitespace, and its new offset given that `s` starts at `offset`.
fn trimmed(s: &str, offset: usize) -> Arg<'_> {
    Arg { text: s.trim(), offset: offset + s.len() - s.trim_start().len() }
}

/// Splits `first, rest` at the first comma; `rest` is `None` without one.
fn split_comma(args: Arg<'_>) -> (Arg<'_>, Option<Arg<'_>>) {
    match args.text.find(',') {
        Some(i) => (
            trimmed(&args.text[..i], args.offset),
            Some(trimmed(&args.text[i + 1..], args.offset + i + 1)).filter(|a| !a.text.is_empty()),
        ),
        None => (args, None),
    }
}

fn span(arg: Arg) -> Range<usize> {
    arg.offset..arg.offset + arg.text.len()
}

fn parse_command(text: &str, offset: usize) -> Result<Command<'_>, SelectionError> {
    let line = trimmed(text, offset);
    let word_len = line.text.find(char::is_whitespace).unwrap_or(line.text.len());
    let word = &line.text[..word_len];
    let args = trimmed(&line.text[word_len..], line.offset + word_len);
    let end = line.offset + line.text.len();

    let rep_and_sel = |what: &str| -> Result<(Reps, Option<Arg>), SelectionError> {
        if args.text.is_empty() {
            return Err(error(end..end, format!("{what} needs a representation: spheres, sticks or cartoon")));
        }
        let (rep, sel) = split_comma(args);
        let reps = Reps::from_name(rep.text)
            .ok_or_else(|| error(span(rep), format!("unknown representation '{}'", rep.text)))?;
        Ok((reps, sel))
    };

    Ok(match word.to_ascii_lowercase().as_str() {
        "select" => {
            if args.text.is_empty() {
                return Err(error(end..end, "select needs a selection"));
            }
            match split_comma(args) {
                (name, Some(sel)) => {
                    let valid = name.text.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                        && name.text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                    if !valid {
                        return Err(error(span(name), "selection names use letters, digits and _"));
                    }
                    Command::Select { name: name.text, sel }
                }
                (_, None) if args.text.contains(',') => return Err(error(end..end, "expected a selection after ','")),
                (sel, None) => Command::Select { name: DEFAULT_SELECTION, sel },
            }
        }
        "deselect" => Command::Deselect,
        "zoom" => Command::Zoom { sel: (!args.text.is_empty()).then_some(args) },
        "show" => {
            let (reps, sel) = rep_and_sel("show")?;
            Command::Show { reps, sel }
        }
        "hide" => {
            let (reps, sel) = rep_and_sel("hide")?;
            Command::Hide { reps, sel }
        }
        "as" => {
            let (reps, sel) = rep_and_sel("as")?;
            Command::As { reps, sel }
        }
        "color" | "colour" => {
            if args.text.is_empty() {
                return Err(error(end..end, "color needs a color name, #rrggbb or a scheme"));
            }
            let (name, sel) = split_comma(args);
            let color = match name.text.to_ascii_lowercase().as_str() {
                "bychain" | "chain" => Color::ByChain,
                "byelement" | "atomic" | "element" => Color::ByElement,
                "byss" | "ss" => Color::BySs,
                "bfactor" | "b" | "spectrum" => Color::BFactor,
                "default" => Color::Default,
                _ => Color::Fixed(
                    color::parse(name.text)
                        .ok_or_else(|| error(span(name), format!("unknown color '{}'", name.text)))?,
                ),
            };
            Command::Color { color, sel }
        }
        _ => Command::Select { name: DEFAULT_SELECTION, sel: line },
    })
}

/// Splits on `;`, keeping each part's byte offset.
fn split_commands(text: &str) -> impl Iterator<Item = (&str, usize)> {
    let mut offset = 0;
    text.split(';').map(move |part| {
        let item = (part, offset);
        offset += part.len() + 1;
        item
    })
}

impl Session {
    /// Loads a structure with the default display and no named selections.
    pub fn load(&mut self, structure: Arc<Structure>) {
        self.display = Some(Display::default_for(&structure));
        self.structure = Some(structure);
        self.named.clear();
    }

    /// Runs one line, which may hold several `;`-separated commands. A dry run reports what the
    /// last command would do; a real run stops at the first error.
    pub fn run(&mut self, text: &str, dry_run: bool) -> Result<Outcome, SelectionError> {
        let mut last = None;
        let mut effects = Vec::new();
        for (part, offset) in split_commands(text).filter(|(p, _)| !p.trim().is_empty()) {
            let mut outcome = self.run_one(part, offset, dry_run)?;
            effects.append(&mut outcome.effects);
            last = Some(outcome);
        }
        let mut outcome = last.ok_or_else(|| error(0..text.len(), "nothing to run"))?;
        outcome.effects = effects;
        Ok(outcome)
    }

    fn run_one(&mut self, text: &str, offset: usize, dry_run: bool) -> Result<Outcome, SelectionError> {
        // `offset` places `text` in the full line, so every error span is absolute.
        let command = parse_command(text, offset)?;
        if let Command::Deselect = command {
            let effects = if dry_run {
                Vec::new()
            } else {
                self.named.remove(DEFAULT_SELECTION);
                vec![Effect::Highlight(Vec::new())]
            };
            return Ok(Outcome { message: "clear the selection".into(), canonical: None, atoms: None, effects });
        }

        let structure = self
            .structure
            .clone()
            .ok_or_else(|| error(offset..offset + text.len(), "no structure loaded"))?;
        let n = structure.atom_count();
        let visible = self.display.as_ref().map(Display::visible);
        let evaluate = |sel: Option<Arg>| -> Result<(AtomSet, Option<String>), SelectionError> {
            let Some(arg) = sel else { return Ok((AtomSet::full(n), None)) };
            // `arg.offset` is absolute; parse errors are relative to the argument text.
            let local = |e: SelectionError| shift(e, arg.offset);
            let parsed = parse(arg.text).map_err(local)?;
            let mut ev = Evaluator::new(&structure, 0).with_named(&self.named);
            if let Some(v) = &visible {
                ev = ev.with_visible(v);
            }
            let set = ev.eval(&parsed).map_err(|e| local(e.into_selection_error(arg.text)))?;
            Ok((set, Some(parsed.to_string())))
        };
        let residues = |set: &AtomSet| set.iter().map(|i| structure.atoms.residue[i]).collect::<BTreeSet<_>>().len();
        let describe = |verb: &str, set: &AtomSet, sel: &Option<String>| match sel {
            Some(_) => format!("{verb} {} atoms in {} residues", set.count(), residues(set)),
            None => format!("{verb} everything"),
        };

        let (message, canonical, set, effects) = match command {
            Command::Select { name, sel } => {
                let (set, canonical) = evaluate(Some(sel))?;
                let message = format!("{name}: {} atoms in {} residues", set.count(), residues(&set));
                let effects = if dry_run {
                    Vec::new()
                } else {
                    let highlight = Effect::Highlight(set.iter().collect());
                    self.named.insert(name.to_owned(), set.clone());
                    vec![highlight]
                };
                (message, canonical, set, effects)
            }
            Command::Zoom { sel } => {
                let (set, canonical) = evaluate(sel)?;
                if let Some(arg) = sel.filter(|_| set.is_empty()) {
                    return Err(error(span(arg), "selection is empty"));
                }
                let target = canonical.as_ref().map(|_| set.iter().collect());
                let message = describe("zoom to", &set, &canonical);
                (message, canonical, set, if dry_run { Vec::new() } else { vec![Effect::Focus(target)] })
            }
            Command::Show { reps, sel } | Command::Hide { reps, sel } | Command::As { reps, sel } => {
                let verb = match command {
                    Command::Show { .. } => "show",
                    Command::Hide { .. } => "hide",
                    _ => "show only",
                };
                let (set, canonical) = evaluate(sel)?;
                let message = describe(&format!("{verb} {}", rep_label(reps)), &set, &canonical);
                if !dry_run {
                    let display = self.display.as_mut().expect("display exists with a structure");
                    match command {
                        Command::Show { .. } => display.show(reps, &set),
                        Command::Hide { .. } => display.hide(reps, &set),
                        _ => display.show_only(reps, &set),
                    }
                }
                (message, canonical, set, if dry_run { Vec::new() } else { vec![Effect::Display] })
            }
            Command::Color { color, sel } => {
                let (set, canonical) = evaluate(sel)?;
                let message = describe("color", &set, &canonical);
                if !dry_run {
                    self.apply_color(&structure, color, &set);
                }
                (message, canonical, set, if dry_run { Vec::new() } else { vec![Effect::Display] })
            }
            Command::Deselect => unreachable!("handled above"),
        };
        Ok(Outcome { message, canonical, atoms: Some(set.count()), effects })
    }

    fn apply_color(&mut self, s: &Structure, color: Color, set: &AtomSet) {
        let display = self.display.as_mut().expect("display exists with a structure");
        match color {
            Color::Fixed(c) => display.color(set, |_| c),
            Color::ByChain => {
                let chains = display::chain_colors(s);
                display.color(set, |i| chains[s.residue_of(i).chain as usize]);
            }
            Color::ByElement => {
                let keep = display.colors.clone();
                display.color(set, |i| color::element(s.atoms.element[i]).filter(|_| display::is_heteroatom(s, i)).unwrap_or(keep[i]));
            }
            Color::BySs => display.color(set, |i| color::secondary_structure(s.residue_of(i).ss)),
            Color::BFactor => {
                // Normalize over the selection so the full ramp is used.
                let (lo, hi) = set.iter().map(|i| s.atoms.b_factor[i]).fold((f32::MAX, f32::MIN), |(lo, hi), b| (lo.min(b), hi.max(b)));
                let range = (hi - lo).max(1e-3);
                display.color(set, |i| color::spectrum((s.atoms.b_factor[i] - lo) / range));
            }
            Color::Default => {
                let defaults = Display::default_for(s).colors;
                display.color(set, |i| defaults[i]);
            }
        }
    }
}

fn shift(e: SelectionError, by: usize) -> SelectionError {
    error(e.span.start + by..e.span.end + by, e.message)
}

fn rep_label(reps: Reps) -> &'static str {
    match reps {
        Reps::SPHERES => "spheres",
        Reps::STICKS => "sticks",
        Reps::CARTOON => "cartoon",
        _ => "everything",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let text = "\
ATOM      1  N   ALA A   1      -1.200   0.500   0.000  1.00 10.00           N
ATOM      2  CA  ALA A   1       0.000   0.000   0.000  1.00 20.00           C
ATOM      3  C   ALA A   1       1.200   0.500   0.000  1.00 30.00           C
ATOM      4  N   ALA A   2       2.400   0.500   0.000  1.00 40.00           N
ATOM      5  CA  ALA A   2       3.800   0.000   0.000  1.00 50.00           C
ATOM      6  C   ALA A   2       5.000   0.500   0.000  1.00 60.00           C
HETATM    7 ZN    ZN A 101       9.000   0.000   0.000  1.00  0.00          ZN
";
        let mut s = Session::default();
        s.load(Arc::new(crate::io::pdb::parse(text, "t").unwrap()));
        s
    }

    fn reps(s: &Session) -> Vec<Reps> {
        s.display.as_ref().unwrap().reps.clone()
    }

    #[test]
    fn select_stores_named_selections() {
        let mut s = session();
        let out = s.run("select metal_site, metals", false).unwrap();
        assert_eq!(out.message, "metal_site: 1 atoms in 1 residues");
        assert_eq!(s.named["metal_site"].iter().collect::<Vec<_>>(), [6]);
        s.run("metal_site or name CA", false).unwrap();
        assert_eq!(s.named["sele"].count(), 3);
        // "sele" is a name, not a command.
        s.run("sele and resi 1", false).unwrap();
        assert_eq!(s.named["sele"].count(), 1);
    }

    #[test]
    fn default_display_and_show_hide_as() {
        let mut s = session();
        let c = Reps::CARTOON;
        assert_eq!(reps(&s), [c, c, c, c, c, c, Reps::STICKS]);
        s.run("show sticks, resi 2", false).unwrap();
        assert_eq!(reps(&s)[3], Reps::CARTOON | Reps::STICKS);
        s.run("hide cartoon", false).unwrap();
        assert_eq!(reps(&s)[..4], [Reps::NONE, Reps::NONE, Reps::NONE, Reps::STICKS]);
        s.run("as spheres, metals", false).unwrap();
        assert_eq!(reps(&s)[6], Reps::SPHERES);
        let out = s.run("hide everything; show sticks, visible or resi 1", false).unwrap();
        assert_eq!(out.effects, [Effect::Display, Effect::Display]);
        // After `hide everything` nothing is visible, so only resi 1 gets sticks.
        assert_eq!(reps(&s).iter().filter(|r| **r == Reps::STICKS).count(), 3);
    }

    #[test]
    fn colors_and_schemes() {
        let mut s = session();
        s.run("color coral, resi 1", false).unwrap();
        let colors = s.display.as_ref().unwrap().colors.clone();
        assert_eq!(colors[0], color::parse("coral").unwrap());
        s.run("color byelement, resi 1", false).unwrap();
        let colors = s.display.as_ref().unwrap().colors.clone();
        assert_eq!(colors[0], color::element(crate::Element::N).unwrap());
        assert_eq!(colors[1], color::parse("coral").unwrap());
        s.run("color bfactor, polymer", false).unwrap();
        let colors = s.display.as_ref().unwrap().colors.clone();
        assert_eq!((colors[0], colors[5]), (color::spectrum(0.0), color::spectrum(1.0)));
        s.run("color #102030", false).unwrap();
        assert!(s.display.as_ref().unwrap().colors.iter().all(|c| *c == [16, 32, 48, 255]));
    }

    #[test]
    fn errors_point_into_the_full_line() {
        let mut s = session();
        let text = "show sticks, name CA; color teal, bogus";
        let err = s.run(text, true).unwrap_err();
        assert_eq!(&text[err.span.clone()], "bogus");
        let text = "show lines";
        assert_eq!(&text[s.run(text, true).unwrap_err().span], "lines");
        let text = "color chartreuse, all";
        assert_eq!(s.run(text, true).unwrap_err().message, "unknown color 'chartreuse'");
        assert_eq!(s.run("zoom resn XYZ", true).unwrap_err().message, "selection is empty");
        assert_eq!(s.run("select 1x, all", true).unwrap_err().message, "selection names use letters, digits and _");
    }

    #[test]
    fn dry_runs_change_nothing() {
        let mut s = session();
        let before = reps(&s);
        let out = s.run("hide everything; select x, all", true).unwrap();
        assert!(out.effects.is_empty());
        assert_eq!(reps(&s), before);
        assert!(s.named.is_empty());
    }
}
