//! A session and its command language: a subset of PyMOL's, shared by every front end.
//!
//!   select [<name>,] <selection>     store (default "sele") and highlight
//!   deselect                         clear "sele" and the highlight
//!   zoom [<selection>]               frame the selection, or everything
//!   show <rep> [, <selection>]       add a representation (spheres, sticks, cartoon, surface)
//!   hide <rep|everything> [, <sel>]  remove representations
//!   as <rep> [, <selection>]         show only that representation
//!   color <color> [, <selection>]    a name or #rrggbb, or a scheme: bychain, byelement,
//!                                    byss, bfactor, default
//!   distance [<name>,] <a>, <b>      measure between two single-atom selections
//!   angle [<name>,] <a>, <b>, <c>    angle at <b>
//!   dihedral [<name>,] <a>, …, <d>   torsion about <b>–<c>
//!   delete <name>|all                remove a measurement or named selection
//!   undo / redo                      step back or forward through changes
//!   <selection>                      same as `select <selection>`
//!
//! Commands separated by `;` run in order. Every line can be dry-run, which evaluates without
//! side effects, for live feedback while typing.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use crate::color::{self, Rgba};
use crate::display::{self, Display, Reps};
use crate::measure::{Measurement, MeasurementKind};
use crate::select::{AtomSet, Evaluator, SelectionError, parse};
use crate::Structure;

const DEFAULT_SELECTION: &str = "sele";

/// One-click looks, as command lines, so front ends can show users the commands they run.
pub const PRESETS: &[(&str, &str)] = &[
    ("Cartoon + ligands", "hide everything; show cartoon; show sticks, not polymer and not solvent"),
    ("Cartoon", "hide everything; show cartoon"),
    ("Surface", "hide everything; show surface, polymer; show sticks, not polymer and not solvent"),
    ("Sticks", "hide everything; show sticks, not solvent"),
    ("Spheres", "hide everything; show spheres, not solvent"),
];

#[derive(Default)]
pub struct Session {
    pub structure: Option<Arc<Structure>>,
    pub display: Option<Display>,
    pub named: HashMap<String, AtomSet>,
    pub measurements: Vec<Measurement>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

/// Everything a command can change; the camera is deliberately not part of undo.
#[derive(Clone, PartialEq)]
struct Snapshot {
    display: Option<Display>,
    named: HashMap<String, AtomSet>,
    measurements: Vec<Measurement>,
}

/// Undo steps kept. A display snapshot is about 5 bytes per atom (1.2 MB for a ribosome).
const UNDO_LIMIT: usize = 50;

/// What a front end must update after a command.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Mark these atoms as selected; empty clears the markers.
    Highlight(Vec<usize>),
    /// Frame these atoms, or everything for `None`.
    Focus(Option<Vec<usize>>),
    /// `Session::display` changed.
    Display,
    /// `Session::measurements` changed.
    Measurements,
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
    Measure { kind: MeasurementKind, name: Option<Arg<'a>>, sels: Vec<Arg<'a>> },
    Delete { name: Arg<'a> },
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

/// Splits on every comma.
fn split_commas(args: Arg<'_>) -> Vec<Arg<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in args.text.char_indices().chain(std::iter::once((args.text.len(), ','))) {
        if c == ',' {
            out.push(trimmed(&args.text[start..i], args.offset + start));
            start = i + 1;
        }
    }
    out
}

fn valid_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
            return Err(error(end..end, format!("{what} needs a representation: spheres, sticks, cartoon or surface")));
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
                    if !valid_name(name.text) {
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
        kw @ ("distance" | "dist" | "angle" | "dihedral" | "dihe") => {
            let kind = match kw {
                "distance" | "dist" => MeasurementKind::Distance,
                "angle" => MeasurementKind::Angle,
                _ => MeasurementKind::Dihedral,
            };
            let n = kind.arity();
            let mut parts = if args.text.is_empty() { Vec::new() } else { split_commas(args) };
            let name = match parts.len() {
                k if k == n => None,
                k if k == n + 1 => Some(parts.remove(0)),
                _ => {
                    let what = format!("{kw} takes [name,] then {n} single-atom selections separated by commas");
                    return Err(error(if args.text.is_empty() { end..end } else { span(args) }, what));
                }
            };
            if let Some(name) = name.filter(|a| !valid_name(a.text)) {
                return Err(error(span(name), "measurement names use letters, digits and _"));
            }
            if let Some(empty) = parts.iter().find(|a| a.text.is_empty()) {
                return Err(error(empty.offset..empty.offset, "expected a selection"));
            }
            Command::Measure { kind, name, sels: parts }
        }
        "delete" => {
            if args.text.is_empty() {
                return Err(error(end..end, "delete needs a measurement or selection name, or all"));
            }
            Command::Delete { name: args }
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
        self.measurements.clear();
        self.undo.clear();
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { display: self.display.clone(), named: self.named.clone(), measurements: self.measurements.clone() }
    }

    fn restore(&mut self, s: Snapshot) -> Vec<Effect> {
        self.display = s.display;
        self.named = s.named;
        self.measurements = s.measurements;
        let sele = self.named.get(DEFAULT_SELECTION).map(|set| set.iter().collect()).unwrap_or_default();
        vec![Effect::Display, Effect::Measurements, Effect::Highlight(sele)]
    }

    /// Steps back (`undo`) or forward through changes.
    fn step(&mut self, undo: bool, dry_run: bool) -> Result<Outcome, SelectionError> {
        let verb = if undo { "undo" } else { "redo" };
        let available = if undo { self.undo.len() } else { self.redo.len() };
        if available == 0 {
            return Err(error(0..verb.len(), format!("nothing to {verb}")));
        }
        let message = format!("{verb} ({} more)", available - 1);
        let effects = if dry_run {
            Vec::new()
        } else {
            let current = self.snapshot();
            let target = if undo { self.undo.pop() } else { self.redo.pop() }.unwrap();
            if undo { self.redo.push(current) } else { self.undo.push(current) }
            self.restore(target)
        };
        Ok(Outcome { message, canonical: None, atoms: None, effects })
    }

    /// Runs one line, which may hold several `;`-separated commands. A dry run reports what the
    /// last command would do; a real run stops at the first error.
    ///
    /// A line that changes anything is one undo step, even when it holds several commands or
    /// fails partway (undo then reverts the commands that did run).
    pub fn run(&mut self, text: &str, dry_run: bool) -> Result<Outcome, SelectionError> {
        match text.trim().to_ascii_lowercase().as_str() {
            "undo" => return self.step(true, dry_run),
            "redo" => return self.step(false, dry_run),
            _ => {}
        }
        let before = (!dry_run).then(|| self.snapshot());
        let mut last = None;
        let mut effects = Vec::new();
        let mut result = Ok(());
        for (part, offset) in split_commands(text).filter(|(p, _)| !p.trim().is_empty()) {
            match self.run_one(part, offset, dry_run) {
                Ok(mut outcome) => {
                    effects.append(&mut outcome.effects);
                    last = Some(outcome);
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        if let Some(before) = before
            && before != self.snapshot()
        {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        result?;
        let mut outcome = last.ok_or_else(|| error(0..text.len(), "nothing to run"))?;
        outcome.effects = effects;
        Ok(outcome)
    }

    fn run_one(&mut self, text: &str, offset: usize, dry_run: bool) -> Result<Outcome, SelectionError> {
        // `offset` places `text` in the full line, so every error span is absolute.
        let command = parse_command(text, offset)?;
        if let Command::Delete { name } = command {
            return self.delete(name, dry_run);
        }
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
            Command::Measure { kind, name, sels } => {
                let mut atoms = Vec::with_capacity(sels.len());
                for &sel in &sels {
                    let (set, _) = evaluate(Some(sel))?;
                    if set.count() != 1 {
                        return Err(error(span(sel), format!("expected exactly one atom, got {}", set.count())));
                    }
                    atoms.push(set.first().unwrap());
                }
                let name = match name {
                    Some(n) => n.text.to_owned(),
                    None => self.next_measurement_name(kind),
                };
                let coords = structure.coords(0).unwrap_or_default();
                let m = Measurement::new(name, kind, atoms, coords);
                let labels: Vec<String> = m.atoms.iter().map(|&i| structure.atom_label(i)).collect();
                let message = format!("{}: {} ({})", m.name, m.label(), labels.join(" – "));
                let set = AtomSet::from_indices(n, m.atoms.iter().copied());
                let effects = if dry_run {
                    Vec::new()
                } else {
                    self.measurements.retain(|x| x.name != m.name);
                    self.measurements.push(m);
                    vec![Effect::Measurements]
                };
                (message, None, set, effects)
            }
            Command::Deselect | Command::Delete { .. } => unreachable!("handled above"),
        };
        Ok(Outcome { message, canonical, atoms: Some(set.count()), effects })
    }

    /// PyMOL-style automatic names: dist01, dist02, …, skipping names already in use.
    fn next_measurement_name(&self, kind: MeasurementKind) -> String {
        (1..)
            .map(|k| format!("{}{k:02}", kind.prefix()))
            .find(|name| self.measurements.iter().all(|m| &m.name != name))
            .unwrap()
    }

    fn delete(&mut self, name: Arg, dry_run: bool) -> Result<Outcome, SelectionError> {
        let outcome = |message: String, effects| Outcome { message, canonical: None, atoms: None, effects };
        if name.text.eq_ignore_ascii_case("all") {
            let message = format!("delete {} measurements and {} selections", self.measurements.len(), self.named.len());
            if dry_run {
                return Ok(outcome(message, Vec::new()));
            }
            self.measurements.clear();
            self.named.clear();
            return Ok(outcome(message, vec![Effect::Measurements, Effect::Highlight(Vec::new())]));
        }
        let is_measurement = self.measurements.iter().any(|m| m.name == name.text);
        if !is_measurement && !self.named.contains_key(name.text) {
            return Err(error(span(name), format!("no measurement or selection named '{}'", name.text)));
        }
        let message = format!("delete {}", name.text);
        if dry_run {
            return Ok(outcome(message, Vec::new()));
        }
        let effects = if is_measurement {
            self.measurements.retain(|m| m.name != name.text);
            vec![Effect::Measurements]
        } else {
            self.named.remove(name.text);
            if name.text == DEFAULT_SELECTION { vec![Effect::Highlight(Vec::new())] } else { Vec::new() }
        };
        Ok(outcome(message, effects))
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
        Reps::SURFACE => "surface",
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
    fn measurements() {
        let mut s = session();
        let out = s.run("distance index 2, index 5", false).unwrap();
        assert_eq!(out.message, "dist01: 3.80 Å (A/ALA 1/CA – A/ALA 2/CA)");
        assert_eq!(out.effects, [Effect::Measurements]);
        s.run("angle bb, index 1, index 2, index 3", false).unwrap();
        s.run("distance index 1, index 3", false).unwrap();
        let names: Vec<_> = s.measurements.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["dist01", "bb", "dist02"]);
        // Re-using a name replaces the measurement.
        s.run("distance bb, index 1, index 2", false).unwrap();
        assert_eq!(s.measurements.len(), 3);
        s.run("delete dist01", false).unwrap();
        assert_eq!(s.measurements.len(), 2);
        s.run("delete all", false).unwrap();
        assert!(s.measurements.is_empty());
    }

    #[test]
    fn measurement_errors() {
        let mut s = session();
        let text = "distance name CA, index 1";
        let err = s.run(text, true).unwrap_err();
        assert_eq!(&text[err.span.clone()], "name CA");
        assert_eq!(err.message, "expected exactly one atom, got 2");
        assert!(s.run("angle index 1, index 2", true).unwrap_err().message.starts_with("angle takes [name,] then 3"));
        assert_eq!(s.run("delete nope", true).unwrap_err().message, "no measurement or selection named 'nope'");
    }

    #[test]
    fn undo_and_redo() {
        let mut s = session();
        let original = reps(&s);
        s.run("hide everything; show sticks, resi 1", false).unwrap();
        let sticks = reps(&s);
        s.run("color coral; distance index 2, index 5", false).unwrap();
        // Camera-only and dry-run commands are not undo steps.
        s.run("zoom resi 1", false).unwrap();
        s.run("show spheres", true).unwrap();

        let out = s.run("undo", false).unwrap();
        assert_eq!(out.message, "undo (1 more)");
        assert!(s.measurements.is_empty());
        assert_eq!(reps(&s), sticks);
        s.run("undo", false).unwrap();
        assert_eq!(reps(&s), original);
        assert_eq!(s.run("undo", false).unwrap_err().message, "nothing to undo");

        s.run("redo", false).unwrap();
        assert_eq!(reps(&s), sticks);
        // A new change discards the redo history.
        s.run("select metals", false).unwrap();
        assert_eq!(s.run("redo", false).unwrap_err().message, "nothing to redo");
        s.run("undo", false).unwrap();
        assert!(!s.named.contains_key("sele"));
    }

    #[test]
    fn partially_failed_lines_are_undoable() {
        let mut s = session();
        let original = reps(&s);
        assert!(s.run("hide everything; show bogus", false).is_err());
        assert_ne!(reps(&s), original);
        s.run("undo", false).unwrap();
        assert_eq!(reps(&s), original);
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
