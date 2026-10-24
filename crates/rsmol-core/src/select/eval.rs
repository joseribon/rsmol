//! Evaluates selection expressions against a structure.
//!
//! Matching follows PyMOL's defaults: atom, residue and element names are case-insensitive,
//! chain IDs are case-sensitive, `*` is a wildcard, `chain` is the author chain ID and `segi`
//! the label (mmCIF `label_asym_id`) ID.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use super::ast::*;
use super::atomset::AtomSet;
use crate::backbone::{self, PolymerKind};
use crate::spatial::Grid;
use crate::{Element, SecondaryStructure, Structure, Vec3};

#[derive(Clone, Debug, PartialEq)]
pub struct EvalError {
    /// Byte range in the selection text, when the error is tied to one name.
    pub span: Option<Range<usize>>,
    pub message: String,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EvalError {}

const PROTEIN_BACKBONE: &[&str] = &["N", "CA", "C", "O", "OXT"];
const NUCLEIC_BACKBONE: &[&str] = &["P", "OP1", "OP2", "OP3", "O1P", "O2P", "O3P", "O5'", "C5'", "C4'", "C3'", "O3'"];

/// Per-structure lookups shared by every expression evaluated against it.
pub struct Evaluator<'a> {
    s: &'a Structure,
    state: usize,
    /// Object names and their atom ranges, in order; one object spanning everything by default.
    objects: Vec<(String, Range<usize>)>,
    named: Option<&'a HashMap<String, AtomSet>>,
    visible: Option<&'a AtomSet>,
    /// Polymer kind of each residue, from the backbone traces.
    polymer: Vec<Option<PolymerKind>>,
    /// Bonded neighbors of each atom, CSR layout.
    adjacency_starts: Vec<u32>,
    adjacency: Vec<u32>,
}

impl<'a> Evaluator<'a> {
    pub fn new(s: &'a Structure, state: usize) -> Self {
        let mut polymer = vec![None; s.residues.len()];
        for t in backbone::traces(s, state) {
            for &r in &t.residues {
                polymer[r as usize] = Some(t.kind);
            }
        }
        let mut degree = vec![0u32; s.atom_count() + 1];
        for b in &s.bonds {
            degree[b.atoms[0] as usize + 1] += 1;
            degree[b.atoms[1] as usize + 1] += 1;
        }
        for i in 1..degree.len() {
            degree[i] += degree[i - 1];
        }
        let mut fill = degree.clone();
        let mut adjacency = vec![0; s.bonds.len() * 2];
        for b in &s.bonds {
            let [a, c] = b.atoms;
            adjacency[fill[a as usize] as usize] = c;
            fill[a as usize] += 1;
            adjacency[fill[c as usize] as usize] = a;
            fill[c as usize] += 1;
        }
        Evaluator {
            s,
            state,
            objects: vec![(s.name.clone(), 0..s.atom_count())],
            named: None,
            visible: None,
            polymer,
            adjacency_starts: degree,
            adjacency,
        }
    }

    /// Named selections that `Expr::Named` can refer to.
    pub fn with_named(mut self, named: &'a HashMap<String, AtomSet>) -> Self {
        self.named = Some(named);
        self
    }

    /// Atoms currently shown, for the `visible` keyword.
    /// The objects a merged scene is made of, as (name, atom range) in order.
    pub fn with_objects(mut self, objects: Vec<(String, Range<usize>)>) -> Self {
        self.objects = objects;
        self
    }

    /// Index into `objects` of the object holding `atom`.
    fn object_of(&self, atom: usize) -> usize {
        self.objects.partition_point(|(_, r)| r.end <= atom).min(self.objects.len().saturating_sub(1))
    }

    pub fn with_visible(mut self, visible: &'a AtomSet) -> Self {
        self.visible = Some(visible);
        self
    }

    pub fn eval(&self, expr: &Expr) -> Result<AtomSet, EvalError> {
        let n = self.s.atom_count();
        let s = self.s;
        let residue = |i: usize| &s.residues[s.atoms.residue[i] as usize];
        Ok(match expr {
            Expr::All => AtomSet::full(n),
            Expr::None => AtomSet::empty(n),
            Expr::Named { name, span } => self.named(name, span)?,
            Expr::Property { prop, patterns } => {
                let matches = |text: &str, case_sensitive: bool| patterns.iter().any(|p| glob(p, text, case_sensitive));
                match prop {
                    Property::Name => AtomSet::from_fn(n, |i| matches(&s.atoms.name[i], false)),
                    Property::Resn => AtomSet::from_fn(n, |i| matches(&residue(i).name, false)),
                    Property::Chain => AtomSet::from_fn(n, |i| matches(&s.chain_of(i).id, true)),
                    Property::Segi => AtomSet::from_fn(n, |i| matches(&s.chain_of(i).label_id, true)),
                    Property::Elem => AtomSet::from_fn(n, |i| matches(s.atoms.element[i].symbol(), false)),
                    Property::Alt => AtomSet::from_fn(n, |i| {
                        matches(&s.atoms.alt_loc[i].map(String::from).unwrap_or_default(), true)
                    }),
                    Property::Ss => AtomSet::from_fn(n, |i| {
                        let code = match residue(i).ss {
                            SecondaryStructure::Helix => "H",
                            SecondaryStructure::Sheet => "S",
                            SecondaryStructure::Loop => "L",
                        };
                        // PyMOL treats an empty ss pattern as loop.
                        patterns.iter().any(|p| glob(p, code, false) || (p.is_empty() && code == "L"))
                    }),
                    Property::Model => {
                        let hit: Vec<bool> = self.objects.iter().map(|(name, _)| matches(name, false)).collect();
                        AtomSet::from_fn(n, |i| hit[self.object_of(i)])
                    }
                }
            }
            Expr::Resi(items) => AtomSet::from_fn(n, |i| {
                let r = residue(i);
                let id = (r.seq_num, r.ins_code);
                items.iter().any(|item| match item {
                    ResiItem::Single(x) => (x.seq, x.ins) == id,
                    // Insertion codes sort after the plain number: 27 < 27A < 27B < 28.
                    ResiItem::Range(lo, hi) => (lo.seq, lo.ins) <= id && id <= (hi.seq, hi.ins),
                })
            }),
            Expr::Index { kind, ranges } => AtomSet::from_fn(n, |i| {
                let value = match kind {
                    IndexKind::Id => s.atoms.serial[i] as i64,
                    IndexKind::Index => i as i64 + 1,
                };
                ranges.iter().any(|&(lo, hi)| lo <= value && value <= hi)
            }),
            Expr::Compare { field, op, value } => AtomSet::from_fn(n, |i| {
                let x = match field {
                    NumericField::B => s.atoms.b_factor[i],
                    NumericField::Q => s.atoms.occupancy[i],
                } as f64;
                match op {
                    CmpOp::Lt => x < *value,
                    CmpOp::Le => x <= *value,
                    CmpOp::Eq => (x - value).abs() < 1e-4,
                    CmpOp::Ge => x >= *value,
                    CmpOp::Gt => x > *value,
                }
            }),
            Expr::Class(class) => self.class(*class)?,
            Expr::State(k) => {
                // States are 1-based in PyMOL; every atom has coordinates in every state here.
                if *k >= 1 && (*k as usize) <= s.state_count() { AtomSet::full(n) } else { AtomSet::empty(n) }
            }
            Expr::Not(e) => self.eval(e)?.complement(),
            Expr::And(a, b) => self.eval(a)?.and(&self.eval(b)?),
            Expr::Or(a, b) => self.eval(a)?.or(&self.eval(b)?),
            Expr::Match { kind, expr, of } => self.matching(*kind, &self.eval(expr)?, &self.eval(of)?),
            Expr::Group { by, expr } => self.group(*by, &self.eval(expr)?),
            Expr::Distance { kind, expr, distance, of } => {
                let (candidates, targets) = (self.eval(expr)?, self.eval(of)?);
                let near = self.near(&targets, *distance, Some(&candidates));
                match kind {
                    DistanceKind::Within => near,
                    DistanceKind::Beyond => candidates.and_not(&near),
                    DistanceKind::NearTo => near.and_not(&targets),
                }
            }
            Expr::Around { expr, distance, include_self } => {
                let center = self.eval(expr)?;
                let near = self.near(&center, *distance, None);
                if *include_self { near.or(&center) } else { near.and_not(&center) }
            }
            Expr::Extend { expr, bonds } => {
                let mut set = self.eval(expr)?;
                for _ in 0..*bonds {
                    set = set.clone().or(&self.bonded(&set));
                }
                set
            }
            Expr::Bonded { expr, include_self } => {
                let set = self.eval(expr)?;
                let neighbors = self.bonded(&set);
                if *include_self { neighbors.or(&set) } else { neighbors.and_not(&set) }
            }
            Expr::First(e) => AtomSet::from_indices(n, self.eval(e)?.first()),
            Expr::Last(e) => AtomSet::from_indices(n, self.eval(e)?.last()),
        })
    }

    fn named(&self, name: &str, span: &Range<usize>) -> Result<AtomSet, EvalError> {
        let n = self.s.atom_count();
        if let Some(set) = self.named.and_then(|m| m.get(name)) {
            return Ok(set.clone());
        }
        if let Some((_, range)) = self.objects.iter().find(|(o, _)| o.eq_ignore_ascii_case(name)) {
            return Ok(AtomSet::from_indices(n, range.clone()));
        }
        Err(EvalError { span: Some(span.clone()), message: format!("no selection or object named '{name}'") })
    }

    fn class(&self, class: Class) -> Result<AtomSet, EvalError> {
        let s = self.s;
        let n = s.atom_count();
        let res = |i: usize| s.atoms.residue[i] as usize;
        let polymer = |i: usize| self.polymer[res(i)];
        let backbone = |i: usize| {
            let names = match polymer(i) {
                Some(PolymerKind::Protein) => PROTEIN_BACKBONE,
                Some(PolymerKind::Nucleic) => NUCLEIC_BACKBONE,
                None => return false,
            };
            names.contains(&s.atoms.name[i].as_str())
        };
        // Non-polymer, non-water residues: organic if they contain carbon, inorganic otherwise.
        let small_molecule = |i: usize| polymer(i).is_none() && !s.is_water(res(i));
        let has_carbon = |r: usize| {
            let atoms = s.residues[r].atoms.clone();
            atoms.into_iter().any(|a| s.atoms.element[a as usize] == Element::C)
        };
        Ok(match class {
            Class::Hetatm => AtomSet::from_fn(n, |i| s.atoms.hetero[i]),
            Class::Hydrogens => AtomSet::from_fn(n, |i| s.atoms.element[i] == Element::H),
            Class::Polymer => AtomSet::from_fn(n, |i| polymer(i).is_some()),
            Class::Protein => AtomSet::from_fn(n, |i| polymer(i) == Some(PolymerKind::Protein)),
            Class::Nucleic => AtomSet::from_fn(n, |i| polymer(i) == Some(PolymerKind::Nucleic)),
            Class::Solvent => AtomSet::from_fn(n, |i| s.is_water(res(i))),
            Class::Organic => AtomSet::from_fn(n, |i| small_molecule(i) && has_carbon(res(i))),
            Class::Inorganic => AtomSet::from_fn(n, |i| small_molecule(i) && !has_carbon(res(i))),
            Class::Metals => AtomSet::from_fn(n, |i| s.atoms.element[i].is_metal()),
            Class::Backbone => AtomSet::from_fn(n, backbone),
            Class::Sidechain => AtomSet::from_fn(n, |i| polymer(i).is_some() && !backbone(i)),
            Class::Visible => match self.visible {
                Some(v) => v.clone(),
                None => return Err(EvalError { span: None, message: "'visible' needs a viewer".into() }),
            },
        })
    }

    /// `in`: atoms of `expr` with the same chain, residue and atom identifiers as an atom of
    /// `of`; `like` ignores the chain. Within one object `in` reduces to intersection, but the
    /// identifier form also works once selections can span objects.
    fn matching(&self, kind: MatchKind, expr: &AtomSet, of: &AtomSet) -> AtomSet {
        let s = self.s;
        let key = |i: usize| {
            let r = s.residue_of(i);
            let chain = if kind == MatchKind::In { s.chain_of(i).id.as_str() } else { "" };
            (chain, r.seq_num, r.ins_code, r.name.as_str(), s.atoms.name[i].as_str())
        };
        let keys: std::collections::HashSet<_> = of.iter().map(key).collect();
        AtomSet::from_indices(s.atom_count(), expr.iter().filter(|&i| keys.contains(&key(i))))
    }

    fn group(&self, by: Grouping, set: &AtomSet) -> AtomSet {
        let s = self.s;
        let n = s.atom_count();
        match by {
            Grouping::Residue => {
                let mut hit = vec![false; s.residues.len()];
                for i in set.iter() {
                    hit[s.atoms.residue[i] as usize] = true;
                }
                AtomSet::from_fn(n, |i| hit[s.atoms.residue[i] as usize])
            }
            Grouping::Segment => {
                let mut hit = vec![false; s.chains.len()];
                for i in set.iter() {
                    hit[s.residue_of(i).chain as usize] = true;
                }
                AtomSet::from_fn(n, |i| hit[s.residue_of(i).chain as usize])
            }
            // Chains are per object: chain A of one structure is not chain A of another.
            Grouping::Chain => {
                let key = |i: usize| (self.object_of(i), s.chain_of(i).id.as_str());
                let ids: std::collections::HashSet<_> = set.iter().map(key).collect();
                AtomSet::from_fn(n, |i| ids.contains(&key(i)))
            }
            Grouping::Object => {
                let hit: std::collections::HashSet<usize> = set.iter().map(|i| self.object_of(i)).collect();
                AtomSet::from_fn(n, |i| hit.contains(&self.object_of(i)))
            }
            Grouping::Molecule => {
                // Connected components over bonds, via union-find with path halving.
                let mut parent: Vec<u32> = (0..n as u32).collect();
                fn root(parent: &mut [u32], mut x: u32) -> u32 {
                    while parent[x as usize] != x {
                        parent[x as usize] = parent[parent[x as usize] as usize];
                        x = parent[x as usize];
                    }
                    x
                }
                for b in &s.bonds {
                    let (a, c) = (root(&mut parent, b.atoms[0]), root(&mut parent, b.atoms[1]));
                    parent[a as usize] = c;
                }
                let mut hit = vec![false; n];
                for i in set.iter() {
                    let r = root(&mut parent, i as u32);
                    hit[r as usize] = true;
                }
                AtomSet::from_fn(n, |i| hit[root(&mut parent, i as u32) as usize])
            }
        }
    }

    /// Atoms (restricted to `candidates`, if given) within `distance` of any atom in `targets`.
    fn near(&self, targets: &AtomSet, distance: f32, candidates: Option<&AtomSet>) -> AtomSet {
        let n = self.s.atom_count();
        let Some(coords) = self.s.coords(self.state) else { return AtomSet::empty(n) };
        let target_coords: Vec<Vec3> = targets.iter().map(|i| coords[i]).collect();
        if target_coords.is_empty() {
            return AtomSet::empty(n);
        }
        let grid = Grid::new(&target_coords, distance.max(1.0));
        let test = |i: usize| grid.any_within(&target_coords, coords[i], distance);
        match candidates {
            Some(c) => AtomSet::from_indices(n, c.iter().filter(|&i| test(i))),
            None => AtomSet::from_fn(n, test),
        }
    }

    /// Atoms bonded to any atom of `set`.
    fn bonded(&self, set: &AtomSet) -> AtomSet {
        let mut out = AtomSet::empty(self.s.atom_count());
        for i in set.iter() {
            let (lo, hi) = (self.adjacency_starts[i] as usize, self.adjacency_starts[i + 1] as usize);
            for &j in &self.adjacency[lo..hi] {
                out.insert(j as usize);
            }
        }
        out
    }
}

/// `*` matches any run of characters; everything else matches literally.
fn glob(pattern: &str, text: &str, case_sensitive: bool) -> bool {
    let eq = |a: u8, b: u8| if case_sensitive { a == b } else { a.eq_ignore_ascii_case(&b) };
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            backtrack = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && eq(p[pi], t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some((star, start)) = backtrack {
            pi = star + 1;
            ti = start + 1;
            backtrack = Some((star, start + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::glob;

    #[test]
    fn glob_patterns() {
        assert!(glob("CA", "ca", false));
        assert!(!glob("CA", "ca", true));
        assert!(glob("C*", "CB", false));
        assert!(glob("*'", "O5'", false));
        assert!(glob("C*2", "CD2", false));
        assert!(!glob("C*2", "CD1", false));
        assert!(glob("", "", true));
        assert!(!glob("", "A", true));
        assert!(glob("*", "", true));
    }
}
