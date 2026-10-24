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
//!   align <mobile>, <target>         superpose mobile's object onto target (sequence + fit)
//!   enable / disable <object>|all    show or hide whole objects
//!   alter <sel>, <field>=<value>,…   set name, resn, resi, chain, segi, elem, alt, b or q
//!   delete <name>|all                remove an object, measurement or named selection
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
use crate::backbone;
use crate::measure::{Measurement, MeasurementKind};
use crate::select::{AtomSet, Evaluator, SelectionError, parse};
use crate::superpose::{self, Transform};
use crate::Structure;

const DEFAULT_SELECTION: &str = "sele";

/// Atom properties `alter` can set. Residue- and chain-level fields (resn, resi, chain, segi)
/// apply to the atom's whole residue or chain segment, which the data model shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomField {
    Name,
    Resn,
    Resi,
    Chain,
    Segi,
    Elem,
    Alt,
    B,
    Q,
}

impl AtomField {
    pub const ALL: [AtomField; 9] = [
        AtomField::Name,
        AtomField::Resn,
        AtomField::Resi,
        AtomField::Chain,
        AtomField::Segi,
        AtomField::Elem,
        AtomField::Alt,
        AtomField::B,
        AtomField::Q,
    ];

    pub fn name(self) -> &'static str {
        match self {
            AtomField::Name => "name",
            AtomField::Resn => "resn",
            AtomField::Resi => "resi",
            AtomField::Chain => "chain",
            AtomField::Segi => "segi",
            AtomField::Elem => "elem",
            AtomField::Alt => "alt",
            AtomField::B => "b",
            AtomField::Q => "q",
        }
    }

    pub fn from_name(name: &str) -> Option<AtomField> {
        AtomField::ALL.into_iter().find(|f| f.name().eq_ignore_ascii_case(name))
    }
}

/// A parsed `alter` value.
enum FieldValue {
    Text(String),
    Number(f32),
    Resi(i32, Option<char>),
    Elem(crate::Element),
    Alt(Option<char>),
}

fn parse_field_value(field: AtomField, raw: &str) -> Result<FieldValue, String> {
    let text = raw.trim();
    let unquoted = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .or_else(|| text.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')))
        .unwrap_or(text);
    Ok(match field {
        AtomField::B | AtomField::Q => {
            FieldValue::Number(unquoted.parse().map_err(|_| format!("{} must be a number, not '{unquoted}'", field.name()))?)
        }
        AtomField::Resi => {
            let digits = unquoted.trim_end_matches(|c: char| c.is_ascii_alphabetic());
            let ins = unquoted[digits.len()..].chars().next();
            let seq = digits.parse().map_err(|_| format!("resi must be a number with an optional insertion code, not '{unquoted}'"))?;
            if unquoted.len() - digits.len() > 1 {
                return Err(format!("insertion codes are one letter, not '{}'", &unquoted[digits.len()..]));
            }
            FieldValue::Resi(seq, ins)
        }
        AtomField::Elem => FieldValue::Elem(
            crate::Element::from_symbol(unquoted).ok_or_else(|| format!("unknown element '{unquoted}'"))?,
        ),
        AtomField::Alt => {
            let mut chars = unquoted.chars();
            let alt = chars.next();
            if chars.next().is_some() {
                return Err(format!("alt locs are one character, not '{unquoted}'"));
            }
            FieldValue::Alt(alt)
        }
        AtomField::Name | AtomField::Resn if unquoted.is_empty() => return Err(format!("{} cannot be empty", field.name())),
        _ => FieldValue::Text(unquoted.to_owned()),
    })
}

/// One-click looks, as command lines, so front ends can show users the commands they run.
pub const PRESETS: &[(&str, &str)] = &[
    ("Cartoon + ligands", "hide everything; show cartoon; show sticks, not polymer and not solvent"),
    ("Cartoon", "hide everything; show cartoon"),
    ("Surface", "hide everything; show surface, polymer; show sticks, not polymer and not solvent"),
    ("Sticks", "hide everything; show sticks, not solvent"),
    ("Spheres", "hide everything; show spheres, not solvent"),
];

/// The file a structure was read from, kept so sessions can embed it.
#[derive(Clone, Debug)]
pub struct Source {
    pub file_name: String,
    pub data: Arc<Vec<u8>>,
}

/// One loaded structure. Its atoms occupy a contiguous range of the session's merged scene.
#[derive(Clone, Debug)]
pub struct Object {
    pub name: String,
    /// Coordinates already include `transform`.
    pub structure: Arc<Structure>,
    pub source: Option<Source>,
    /// Applied to the file's coordinates (e.g. by `align`); saved so sessions can reproduce it.
    pub transform: Transform,
    pub enabled: bool,
}

/// A session holds any number of objects. `structure` is their merged scene: every selection,
/// display flag, measurement and atom index refers to it, and objects are named ranges of it.
#[derive(Default)]
pub struct Session {
    pub objects: Vec<Object>,
    pub structure: Option<Arc<Structure>>,
    pub display: Option<Display>,
    pub named: HashMap<String, AtomSet>,
    pub measurements: Vec<Measurement>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

/// Everything a command can change; the camera is deliberately not part of undo.
#[derive(Clone)]
struct Snapshot {
    objects: Vec<Object>,
    display: Option<Display>,
    named: HashMap<String, AtomSet>,
    measurements: Vec<Measurement>,
}

impl PartialEq for Snapshot {
    fn eq(&self, other: &Snapshot) -> bool {
        let same_objects = self.objects.len() == other.objects.len()
            && self.objects.iter().zip(&other.objects).all(|(a, b)| {
                a.name == b.name && Arc::ptr_eq(&a.structure, &b.structure) && a.enabled == b.enabled
            });
        same_objects && self.display == other.display && self.named == other.named && self.measurements == other.measurements
    }
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
    /// `Session::structure` was replaced (objects added, removed or moved); `refit` asks the
    /// front end to frame everything.
    Scene { refit: bool },
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
    /// Numbers a command produced, for scripting (e.g. `rmsd` from align, `value` from distance).
    pub values: Vec<(&'static str, f64)>,
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
    Align { mobile: Arg<'a>, target: Arg<'a> },
    Alter { sel: Arg<'a>, assignments: Vec<(AtomField, Arg<'a>)> },
    Enable { name: Arg<'a>, enabled: bool },
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
                return Err(error(end..end, "delete needs an object, measurement or selection name, or all"));
            }
            Command::Delete { name: args }
        }
        "align" | "super" => match split_comma(args) {
            (mobile, Some(target)) if !mobile.text.is_empty() => Command::Align { mobile, target },
            _ => return Err(error(if args.text.is_empty() { end..end } else { span(args) }, "align takes <mobile>, <target>")),
        },
        "alter" => {
            let mut parts = if args.text.is_empty() { Vec::new() } else { split_commas(args) };
            if parts.len() < 2 || parts[0].text.is_empty() {
                return Err(error(if args.text.is_empty() { end..end } else { span(args) }, "alter takes <selection>, <field>=<value>[, …]"));
            }
            let sel = parts.remove(0);
            let mut assignments = Vec::new();
            for part in parts {
                let Some(eq) = part.text.find('=') else {
                    return Err(error(span(part), "expected <field>=<value>, e.g. b=0 or chain=B"));
                };
                let field_arg = trimmed(&part.text[..eq], part.offset);
                let field = AtomField::from_name(field_arg.text).ok_or_else(|| {
                    let names: Vec<_> = AtomField::ALL.iter().map(|f| f.name()).collect();
                    error(span(field_arg), format!("cannot alter '{}'; fields are {}", field_arg.text, names.join(", ")))
                })?;
                assignments.push((field, trimmed(&part.text[eq + 1..], part.offset + eq + 1)));
            }
            Command::Alter { sel, assignments }
        }
        kw @ ("enable" | "disable") => {
            if args.text.is_empty() {
                return Err(error(end..end, format!("{kw} needs an object name or all")));
            }
            Command::Enable { name: args, enabled: kw == "enable" }
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

fn transformed(s: &Structure, t: &Transform) -> Structure {
    let mut out = s.clone();
    for state in &mut out.states {
        for p in state.iter_mut() {
            *p = t.apply(*p);
        }
    }
    out
}

impl Session {
    /// Replaces everything with one structure read from `source`, so it can later be saved.
    pub fn load_with_source(&mut self, structure: Arc<Structure>, source: Source) {
        self.load_object(structure, Some(source));
    }

    /// Replaces everything with one structure: default display, no selections, no history.
    pub fn load(&mut self, structure: Arc<Structure>) {
        self.load_object(structure, None);
    }

    fn load_object(&mut self, structure: Arc<Structure>, source: Option<Source>) {
        self.display = Some(Display::default_for(&structure));
        self.objects = vec![Object {
            name: structure.name.clone(),
            structure,
            source,
            transform: Transform::IDENTITY,
            enabled: true,
        }];
        self.rebuild_scene();
        self.named.clear();
        self.measurements.clear();
        self.undo.clear();
        self.redo.clear();
    }

    /// Adds a structure as a new object (or loads it, if the session is empty) and returns its
    /// name, made unique if needed, plus the effects to apply. Undoable.
    pub fn add_object(&mut self, structure: Arc<Structure>, source: Option<Source>) -> (String, Vec<Effect>) {
        if self.objects.is_empty() {
            let name = structure.name.clone();
            self.load_object(structure, source);
            return (name, vec![Effect::Scene { refit: true }, Effect::Display, Effect::Measurements]);
        }
        let before = self.snapshot();
        let base = structure.name.clone();
        let name = std::iter::once(base.clone())
            .chain((2..).map(|k| format!("{base}_{k}")))
            .find(|n| self.objects.iter().all(|o| !o.name.eq_ignore_ascii_case(n)))
            .unwrap();
        let palette_offset: usize = self.objects.iter().map(|o| display::chain_count(&o.structure)).sum();
        let added = Display::default_with_palette_offset(&structure, palette_offset);
        let display = self.display.as_mut().expect("display exists with objects");
        display.reps.extend(added.reps);
        display.colors.extend(added.colors);
        self.objects.push(Object { name: name.clone(), structure, source, transform: Transform::IDENTITY, enabled: true });
        self.rebuild_scene();
        let n = self.atom_count();
        for set in self.named.values_mut() {
            *set = set.resized(n);
        }
        self.record(before);
        (name, vec![Effect::Scene { refit: true }, Effect::Display, Effect::Measurements])
    }

    fn atom_count(&self) -> usize {
        self.structure.as_ref().map_or(0, |s| s.atom_count())
    }

    /// Object names and their atom ranges in the merged scene.
    pub fn object_ranges(&self) -> Vec<(String, Range<usize>)> {
        let mut start = 0;
        self.objects
            .iter()
            .map(|o| {
                let range = start..start + o.structure.atom_count();
                start = range.end;
                (o.name.clone(), range)
            })
            .collect()
    }

    /// The display as drawn: like `display`, with disabled objects hidden.
    pub fn render_display(&self) -> Option<Display> {
        let mut display = self.display.clone()?;
        for ((_, range), object) in self.object_ranges().into_iter().zip(&self.objects) {
            if !object.enabled {
                for r in &mut display.reps[range] {
                    *r = Reps::NONE;
                }
            }
        }
        Some(display)
    }

    /// Rebuilds the merged scene from the objects. One object is used as is, without a copy.
    pub(crate) fn rebuild_scene(&mut self) {
        self.structure = match self.objects.as_slice() {
            [] => None,
            [only] => Some(only.structure.clone()),
            many => {
                let parts: Vec<&Structure> = many.iter().map(|o| o.structure.as_ref()).collect();
                Some(Arc::new(Structure::concat("scene", &parts)))
            }
        };
        if self.structure.is_none() {
            self.display = None;
        }
    }

    /// Recomputes measurement values after coordinates moved.
    fn remeasure(&mut self) {
        let Some(s) = self.structure.clone() else { return };
        let coords = s.coords(0).unwrap_or_default();
        for m in &mut self.measurements {
            *m = Measurement::new(std::mem::take(&mut m.name), m.kind, std::mem::take(&mut m.atoms), coords);
        }
    }

    /// Removes object `k`, remapping display, selections and measurements onto the new scene.
    fn remove_object(&mut self, k: usize) {
        let range = self.object_ranges()[k].1.clone();
        let width = range.len();
        let map = |i: usize| match i {
            i if i < range.start => Some(i),
            i if i >= range.end => Some(i - width),
            _ => None,
        };
        self.objects.remove(k);
        if let Some(display) = self.display.as_mut() {
            display.reps.drain(range.clone());
            display.colors.drain(range.clone());
        }
        self.rebuild_scene();
        let n = self.atom_count();
        for set in self.named.values_mut() {
            *set = AtomSet::from_indices(n, set.iter().filter_map(map));
        }
        self.measurements.retain(|m| m.atoms.iter().all(|&a| map(a).is_some()));
        for m in &mut self.measurements {
            m.atoms = m.atoms.iter().map(|&a| map(a).unwrap()).collect();
        }
        self.remeasure();
    }

    /// Sets atom properties, one `(atom, field, value)` per change; values are parsed as the
    /// command language does (numbers for b/q, `27A` for resi, element symbols, …). One undo
    /// step. Altered objects re-embed their current structure, so saved sessions keep the edits.
    pub fn alter(&mut self, changes: &[(usize, AtomField, String)]) -> Result<Vec<Effect>, String> {
        let before = self.snapshot();
        let effects = self.apply_alter(changes)?;
        self.record(before);
        Ok(effects)
    }

    fn apply_alter(&mut self, changes: &[(usize, AtomField, String)]) -> Result<Vec<Effect>, String> {
        let n = self.atom_count();
        let parsed: Vec<(usize, FieldValue)> = changes
            .iter()
            .map(|(i, field, raw)| {
                if *i >= n {
                    return Err(format!("atom index {} out of range", i + 1));
                }
                parse_field_value(*field, raw).map(|v| (*i, v))
            })
            .collect::<Result<_, _>>()?;
        let fields: Vec<AtomField> = changes.iter().map(|c| c.1).collect();
        if parsed.is_empty() {
            return Ok(Vec::new());
        }
        let ranges = self.object_ranges();
        let mut edited: Vec<Option<Structure>> = vec![None; self.objects.len()];
        for ((i, value), field) in parsed.into_iter().zip(fields) {
            let k = ranges.partition_point(|(_, r)| r.end <= i);
            let local = i - ranges[k].1.start;
            let s = edited[k].get_or_insert_with(|| (*self.objects[k].structure).clone());
            let residue = s.atoms.residue[local] as usize;
            let chain = s.residues[residue].chain as usize;
            match (field, value) {
                (AtomField::Name, FieldValue::Text(t)) => s.atoms.name[local] = t,
                (AtomField::Resn, FieldValue::Text(t)) => s.residues[residue].name = t,
                (AtomField::Chain, FieldValue::Text(t)) => s.chains[chain].id = t,
                (AtomField::Segi, FieldValue::Text(t)) => s.chains[chain].label_id = t,
                (AtomField::Resi, FieldValue::Resi(seq, ins)) => {
                    s.residues[residue].seq_num = seq;
                    s.residues[residue].ins_code = ins;
                }
                (AtomField::Elem, FieldValue::Elem(e)) => s.atoms.element[local] = e,
                (AtomField::Alt, FieldValue::Alt(a)) => s.atoms.alt_loc[local] = a,
                (AtomField::B, FieldValue::Number(v)) => s.atoms.b_factor[local] = v,
                (AtomField::Q, FieldValue::Number(v)) => s.atoms.occupancy[local] = v,
                _ => unreachable!("parse_field_value returns the variant for its field"),
            }
        }
        for (k, structure) in edited.into_iter().enumerate() {
            let Some(structure) = structure else { continue };
            let object = &mut self.objects[k];
            // The embedded file must now carry the edits; its coordinates already include
            // `transform`, so that resets to identity.
            let all: Vec<usize> = (0..structure.atom_count()).collect();
            let data = crate::io::write::to_mmcif(&structure, &all).into_bytes();
            object.source = Some(Source { file_name: format!("{}.cif", object.name), data: Arc::new(data) });
            object.transform = Transform::IDENTITY;
            object.structure = Arc::new(structure);
        }
        self.rebuild_scene();
        Ok(vec![Effect::Scene { refit: false }, Effect::Display])
    }

    /// The selected atoms as PDB (`format` "pdb"/"ent") or mmCIF ("cif"/"mmcif") text.
    pub fn export(&self, format: &str, selection: &str) -> Result<String, String> {
        let set = self.select_atoms(selection).map_err(|e| e.render(selection))?;
        if set.is_empty() {
            return Err("nothing to save: the selection is empty".into());
        }
        let structure = self.structure.as_ref().expect("a selection matched, so a structure is loaded");
        let atoms: Vec<usize> = set.iter().collect();
        match format.to_ascii_lowercase().as_str() {
            "pdb" | "ent" => crate::io::write::to_pdb(structure, &atoms),
            "cif" | "mmcif" => Ok(crate::io::write::to_mmcif(structure, &atoms)),
            other => Err(format!("cannot save '.{other}' files; use .pdb, .cif or .rsmol")),
        }
    }

    /// Evaluates a selection against the scene, with named selections, objects and `visible`.
    pub fn select_atoms(&self, text: &str) -> Result<AtomSet, SelectionError> {
        let structure = self.structure.as_ref().ok_or_else(|| error(0..text.len(), "no structure loaded"))?;
        let expr = parse(text)?;
        let visible = self.render_display().as_ref().map(Display::visible);
        let mut ev = Evaluator::new(structure, 0).with_named(&self.named).with_objects(self.object_ranges());
        if let Some(v) = &visible {
            ev = ev.with_visible(v);
        }
        ev.eval(&expr).map_err(|e| e.into_selection_error(text))
    }

    /// Pushes `before` as an undo step if anything changed since.
    fn record(&mut self, before: Snapshot) {
        if before != self.snapshot() {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            objects: self.objects.clone(),
            display: self.display.clone(),
            named: self.named.clone(),
            measurements: self.measurements.clone(),
        }
    }

    fn restore(&mut self, s: Snapshot) -> Vec<Effect> {
        let scene_changed = s.objects.len() != self.objects.len()
            || s.objects.iter().zip(&self.objects).any(|(a, b)| !Arc::ptr_eq(&a.structure, &b.structure));
        self.objects = s.objects;
        if scene_changed {
            self.rebuild_scene();
        }
        self.display = s.display;
        self.named = s.named;
        self.measurements = s.measurements;
        let sele = self.named.get(DEFAULT_SELECTION).map(|set| set.iter().collect()).unwrap_or_default();
        let mut effects = vec![Effect::Display, Effect::Measurements, Effect::Highlight(sele)];
        if scene_changed {
            effects.insert(0, Effect::Scene { refit: false });
        }
        effects
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
        Ok(Outcome { message, canonical: None, atoms: None, effects, values: Vec::new() })
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
        if let Some(before) = before {
            self.record(before);
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
            return Ok(Outcome { message: "clear the selection".into(), canonical: None, atoms: None, effects, values: Vec::new() });
        }

        let structure = self
            .structure
            .clone()
            .ok_or_else(|| error(offset..offset + text.len(), "no structure loaded"))?;
        let n = structure.atom_count();
        let visible = self.render_display().as_ref().map(Display::visible);
        let objects = self.object_ranges();
        let evaluate = |sel: Option<Arg>| -> Result<(AtomSet, Option<String>), SelectionError> {
            let Some(arg) = sel else { return Ok((AtomSet::full(n), None)) };
            // `arg.offset` is absolute; parse errors are relative to the argument text.
            let local = |e: SelectionError| shift(e, arg.offset);
            let parsed = parse(arg.text).map_err(local)?;
            let mut ev = Evaluator::new(&structure, 0).with_named(&self.named).with_objects(objects.clone());
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

        let mut values = Vec::new();
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
                values.push(("value", m.value as f64));
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
            Command::Align { mobile, target } => {
                let (mobile_set, _) = evaluate(Some(mobile))?;
                let (target_set, _) = evaluate(Some(target))?;
                let object_of = |i: usize| objects.partition_point(|(_, r)| r.end <= i);
                let owners = |set: &AtomSet| set.iter().map(object_of).collect::<BTreeSet<_>>();
                let mobile_objects = owners(&mobile_set);
                let k = match mobile_objects.iter().collect::<Vec<_>>().as_slice() {
                    [k] => **k,
                    [] => return Err(error(span(mobile), "mobile selection is empty")),
                    _ => return Err(error(span(mobile), "mobile selection spans several objects; align moves one")),
                };
                if owners(&target_set).contains(&k) {
                    return Err(error(span(target), "target overlaps the object being moved"));
                }
                // Pair residues by their guide atoms (CA / C4') inside each selection.
                let coords = structure.coords(0).unwrap_or_default();
                let traces = backbone::traces(&structure, 0);
                let residues = |set: &AtomSet| -> Vec<([f32; 3], u8)> {
                    traces
                        .iter()
                        .flat_map(|t| t.residues.iter().zip(&t.guide_atoms))
                        .filter(|(_, g)| set.contains(**g as usize))
                        .map(|(&r, &g)| (coords[g as usize], superpose::one_letter(&structure.residues[r as usize].name)))
                        .collect()
                };
                let result = superpose::align(&residues(&mobile_set), &residues(&target_set)).ok_or_else(|| {
                    error(offset..offset + text.len(), "fewer than 3 residues could be paired between the selections")
                })?;
                let name = self.objects[k].name.clone();
                values.extend([
                    ("rmsd", result.rmsd as f64),
                    ("aligned", result.aligned as f64),
                    ("paired", result.paired as f64),
                    ("cycles", result.cycles as f64),
                ]);
                let message = format!(
                    "{name}: RMSD = {:.3} Å over {} residues ({} paired, {} cycles)",
                    result.rmsd, result.aligned, result.paired, result.cycles
                );
                let effects = if dry_run {
                    Vec::new()
                } else {
                    let object = &mut self.objects[k];
                    object.structure = Arc::new(transformed(&object.structure, &result.transform));
                    object.transform = result.transform.after(&object.transform);
                    self.rebuild_scene();
                    self.remeasure();
                    vec![Effect::Scene { refit: false }, Effect::Measurements]
                };
                (message, None, mobile_set, effects)
            }
            Command::Alter { sel, assignments } => {
                let (set, canonical) = evaluate(Some(sel))?;
                let mut changes = Vec::new();
                for (field, value) in &assignments {
                    parse_field_value(*field, value.text).map_err(|m| error(span(*value), m))?;
                    changes.extend(set.iter().map(|i| (i, *field, value.text.to_owned())));
                }
                let summary: Vec<String> = assignments.iter().map(|(f, v)| format!("{}={}", f.name(), v.text)).collect();
                let message = format!("alter {} atoms: {}", set.count(), summary.join(", "));
                let effects = if dry_run {
                    Vec::new()
                } else {
                    self.apply_alter(&changes).map_err(|m| error(span(sel), m))?
                };
                (message, canonical, set, effects)
            }
            Command::Enable { name, enabled } => {
                let all = name.text.eq_ignore_ascii_case("all");
                let hits: Vec<usize> =
                    (0..self.objects.len()).filter(|&k| all || self.objects[k].name.eq_ignore_ascii_case(name.text)).collect();
                if hits.is_empty() {
                    return Err(error(span(name), format!("no object named '{}'", name.text)));
                }
                if !dry_run {
                    for &k in &hits {
                        self.objects[k].enabled = enabled;
                    }
                }
                let set = AtomSet::from_indices(n, hits.iter().flat_map(|&k| objects[k].1.clone()));
                let verb = if enabled { "enable" } else { "disable" };
                (format!("{verb} {}", if all { "all objects".to_owned() } else { name.text.to_owned() }), None, set,
                    if dry_run { Vec::new() } else { vec![Effect::Display] })
            }
            Command::Deselect | Command::Delete { .. } => unreachable!("handled above"),
        };
        Ok(Outcome { message, canonical, atoms: Some(set.count()), effects, values })
    }

    /// PyMOL-style automatic names: dist01, dist02, …, skipping names already in use.
    fn next_measurement_name(&self, kind: MeasurementKind) -> String {
        (1..)
            .map(|k| format!("{}{k:02}", kind.prefix()))
            .find(|name| self.measurements.iter().all(|m| &m.name != name))
            .unwrap()
    }

    fn delete(&mut self, name: Arg, dry_run: bool) -> Result<Outcome, SelectionError> {
        let outcome = |message: String, effects| Outcome { message, canonical: None, atoms: None, effects, values: Vec::new() };
        if name.text.eq_ignore_ascii_case("all") {
            let message = format!(
                "delete {} objects, {} measurements and {} selections",
                self.objects.len(),
                self.measurements.len(),
                self.named.len()
            );
            if dry_run {
                return Ok(outcome(message, Vec::new()));
            }
            self.objects.clear();
            self.rebuild_scene();
            self.measurements.clear();
            self.named.clear();
            let effects = vec![Effect::Scene { refit: true }, Effect::Measurements, Effect::Highlight(Vec::new())];
            return Ok(outcome(message, effects));
        }
        if let Some(k) = self.objects.iter().position(|o| o.name.eq_ignore_ascii_case(name.text)) {
            let message = format!("delete object {}", self.objects[k].name);
            if dry_run {
                return Ok(outcome(message, Vec::new()));
            }
            self.remove_object(k);
            let sele = self.named.get(DEFAULT_SELECTION).map(|set| set.iter().collect()).unwrap_or_default();
            let effects = vec![Effect::Scene { refit: true }, Effect::Display, Effect::Measurements, Effect::Highlight(sele)];
            return Ok(outcome(message, effects));
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
    fn alter_sets_properties_and_is_undoable() {
        let mut s = session();
        let out = s.run("alter resi 1, b=12.5, chain=Z", false).unwrap();
        assert_eq!(out.message, "alter 3 atoms: b=12.5, chain=Z");
        let st = s.structure.clone().unwrap();
        assert_eq!(st.atoms.b_factor[0], 12.5);
        assert_eq!(st.chain_of(0).id, "Z");
        assert_eq!(s.run("chain Z", true).unwrap().atoms, Some(7)); // chain is per segment: all of A
        s.run("alter index 2, name=\"CX\", resi=27A, elem=N", false).unwrap();
        let st = s.structure.clone().unwrap();
        assert_eq!((st.atoms.name[1].as_str(), st.residue_of(1).seq_num, st.residue_of(1).ins_code), ("CX", 27, Some('A')));
        assert_eq!(st.atoms.element[1], crate::Element::N);
        // The embedded source now carries the edits.
        let source = s.objects[0].source.as_ref().unwrap();
        assert!(String::from_utf8_lossy(&source.data).contains(" CX "));
        s.run("undo", false).unwrap();
        assert_eq!(s.structure.as_ref().unwrap().atoms.name[1], "CA");
    }

    #[test]
    fn alter_errors() {
        let mut s = session();
        let text = "alter all, b=high";
        let err = s.run(text, true).unwrap_err();
        assert_eq!(&text[err.span.clone()], "high");
        assert_eq!(err.message, "b must be a number, not 'high'");
        let text = "alter all, color=red";
        assert_eq!(&text[s.run(text, true).unwrap_err().span], "color");
        assert!(s.run("alter all, elem=Xx", true).unwrap_err().message.contains("unknown element"));
        assert!(s.run("alter all", true).is_err());
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

#[cfg(test)]
mod object_tests {
    use super::*;
    use crate::superpose::Transform;

    fn traces() -> Structure {
        crate::io::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/traces.pdb")).unwrap()
    }

    /// traces.pdb plus a rotated, shifted copy named "moved".
    fn two_objects() -> Session {
        let mut s = Session::default();
        s.add_object(Arc::new(traces()), None);
        let (sn, cs) = 0.7f32.sin_cos();
        let motion = Transform { rotation: [[cs, -sn, 0.0], [sn, cs, 0.0], [0.0, 0.0, 1.0]], translation: [20.0, 5.0, -3.0] };
        let mut copy = transformed(&traces(), &motion);
        copy.name = "moved".into();
        let (name, effects) = s.add_object(Arc::new(copy), None);
        assert_eq!(name, "moved");
        assert_eq!(effects[0], Effect::Scene { refit: true });
        s
    }

    #[test]
    fn objects_are_named_ranges_of_the_scene() {
        let mut s = two_objects();
        assert_eq!(s.structure.as_ref().unwrap().atom_count(), 54);
        assert_eq!(s.run("select moved", false).unwrap().atoms, Some(27));
        assert_eq!(s.run("model traces and name CA", true).unwrap().atoms, Some(6));
        // bychain stays inside one object even though both have a chain A.
        assert_eq!(s.run("bychain (moved and resi 1 and chain A)", true).unwrap().atoms, Some(21));
        assert_eq!(s.run("byobject index 30", true).unwrap().atoms, Some(27));
        // A second copy of the same file gets a unique name.
        let (name, _) = s.add_object(Arc::new(traces()), None);
        assert_eq!(name, "traces_2");
    }

    #[test]
    fn disable_hides_without_losing_the_display() {
        let mut s = two_objects();
        s.run("disable moved", false).unwrap();
        let drawn = s.render_display().unwrap();
        assert!(drawn.reps[27..].iter().all(|r| *r == Reps::NONE));
        assert!(s.display.as_ref().unwrap().reps[27..].iter().any(|r| *r != Reps::NONE));
        // Only the enabled object counts as visible: all 27 atoms of "traces" are shown.
        assert_eq!(s.run("visible", true).unwrap().atoms, Some(27));
        s.run("enable all", false).unwrap();
        assert!(s.render_display().unwrap().reps[27..].iter().any(|r| *r != Reps::NONE));
        assert!(s.run("disable nope", true).is_err());
    }

    #[test]
    fn deleting_an_object_remaps_everything() {
        let mut s = two_objects();
        s.run("select ends, index 1+54", false).unwrap();
        s.run("distance index 2, index 6; distance index 30, index 34", false).unwrap();
        s.run("delete traces", false).unwrap();
        assert_eq!(s.objects.len(), 1);
        assert_eq!(s.structure.as_ref().unwrap().atom_count(), 27);
        // Index 54 (last atom of "moved") is now 27; index 1 belonged to the deleted object.
        assert_eq!(s.named["ends"].iter().collect::<Vec<_>>(), [26]);
        // Only the measurement inside "moved" survives, renumbered.
        assert_eq!(s.measurements.len(), 1);
        assert_eq!(s.measurements[0].atoms, [2, 6]);
        s.run("undo", false).unwrap();
        assert_eq!(s.objects.len(), 2);
        assert_eq!(s.measurements.len(), 2);
    }

    #[test]
    fn align_superposes_the_mobile_object() {
        let mut s = two_objects();
        let before = s.run("align moved, traces", true).unwrap();
        assert!(before.message.starts_with("moved: RMSD = 0.000 Å over 7 residues"), "{}", before.message);
        s.run("distance index 2, index 29", false).unwrap();
        assert!(s.measurements[0].value > 5.0);
        let out = s.run("align moved, traces", false).unwrap();
        assert_eq!(out.effects[0], Effect::Scene { refit: false });
        // The copy now sits on the original: CA 1 of each coincide.
        assert!(s.measurements[0].value < 1e-3, "{}", s.measurements[0].value);
        assert_ne!(s.objects[1].transform, Transform::IDENTITY);
        s.run("undo", false).unwrap();
        assert!(s.measurements[0].value > 5.0);
        assert!(s.run("align traces or moved, traces", true).unwrap_err().message.contains("several objects"));
    }
}
