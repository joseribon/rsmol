//! Backbone traces: the runs of consecutive polymer residues that cartoon representations
//! follow.
//!
//! Proteins are traced through CA atoms, with the carbonyl O giving the ribbon orientation;
//! nucleic acids through C4' atoms (P for phosphate-only models). A trace breaks at chain
//! boundaries, non-polymer residues and gaps (missing residues), detected from guide-atom
//! distances in the given state.

use crate::{SecondaryStructure, Structure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolymerKind {
    Protein,
    Nucleic,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    pub kind: PolymerKind,
    /// Index into `Structure::chains`.
    pub chain: u32,
    pub residues: Vec<u32>,
    /// Guide atom per residue: CA (protein) or C4' (nucleic).
    pub guide_atoms: Vec<u32>,
    /// Carbonyl O per residue for proteins, used to orient ribbons; `None` if absent.
    pub orient_atoms: Vec<Option<u32>>,
}

impl Trace {
    pub fn len(&self) -> usize {
        self.residues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.residues.is_empty()
    }

    pub fn secondary_structure<'a>(&'a self, s: &'a Structure) -> impl Iterator<Item = SecondaryStructure> + 'a {
        self.residues.iter().map(|&r| s.residues[r as usize].ss)
    }
}

/// Maximum guide-atom distance (Å) between consecutive residues before the trace breaks.
/// CA–CA is 3.8 Å (2.9 for cis-peptides); C4'–C4' is about 6–7 Å.
fn max_step(kind: PolymerKind) -> f32 {
    match kind {
        PolymerKind::Protein => 4.3,
        PolymerKind::Nucleic => 8.0,
    }
}

struct Guide {
    kind: PolymerKind,
    atom: u32,
    orient: Option<u32>,
}

const AMINO_ACIDS: &[&str] = &[
    "ALA", "ARG", "ASN", "ASP", "CYS", "GLN", "GLU", "GLY", "HIS", "ILE", "LEU", "LYS", "MET",
    "PHE", "PRO", "SER", "THR", "TRP", "TYR", "VAL", "MSE", "SEC", "PYL",
];
const NUCLEOTIDES: &[&str] = &["A", "C", "G", "U", "I", "DA", "DC", "DG", "DT", "DI", "DU"];

/// Classifies a residue by its backbone atoms.
///
/// Full backbones need CA with N and C (which keeps calcium ions, also named "CA", out) or C4'
/// with any other sugar/phosphate atom (models are often truncated, e.g. missing C1').
/// Coarse-grained models with only CA or P per residue are accepted for standard residue names.
fn guide(s: &Structure, residue: usize) -> Option<Guide> {
    let r = &s.residues[residue];
    let atoms = r.atoms.clone();
    // First match wins, which picks alt loc A over B for disordered backbones.
    let find = |names: &[&str]| atoms.clone().find(|&a| names.contains(&s.atoms.name[a as usize].as_str()));
    let protein = |atom, orient| Some(Guide { kind: PolymerKind::Protein, atom, orient });
    let nucleic = |atom| Some(Guide { kind: PolymerKind::Nucleic, atom, orient: None });

    if let Some(ca) = find(&["CA"]) {
        if find(&["N"]).is_some() && find(&["C"]).is_some() {
            return protein(ca, find(&["O"]));
        }
        if r.atoms.len() == 1 && AMINO_ACIDS.contains(&r.name.as_str()) {
            return protein(ca, None);
        }
    }
    // Older files use `*` instead of `'` in sugar atom names.
    if let Some(c4) = find(&["C4'", "C4*"])
        && find(&["C1'", "C3'", "O4'", "C5'", "P", "C1*", "C3*", "O4*", "C5*"]).is_some()
    {
        return nucleic(c4);
    }
    if let Some(p) = find(&["P"])
        && NUCLEOTIDES.contains(&r.name.as_str())
    {
        return nucleic(p);
    }
    None
}

pub fn traces(s: &Structure, state: usize) -> Vec<Trace> {
    let Some(coords) = s.coords(state) else { return Vec::new() };
    let dist = |a: u32, b: u32| {
        let (p, q) = (coords[a as usize], coords[b as usize]);
        ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
    };

    let mut out = Vec::new();
    let mut current: Option<Trace> = None;
    let flush = |t: Option<Trace>, out: &mut Vec<Trace>| {
        if let Some(t) = t.filter(|t| t.len() >= 2) {
            out.push(t);
        }
    };

    for (chain_index, chain) in s.chains.iter().enumerate() {
        for residue in chain.residues.clone() {
            let Some(g) = guide(s, residue as usize) else {
                flush(current.take(), &mut out);
                continue;
            };
            let continues = current.as_ref().is_some_and(|t| {
                t.kind == g.kind && dist(*t.guide_atoms.last().unwrap(), g.atom) <= max_step(g.kind)
            });
            if !continues {
                flush(current.take(), &mut out);
                current = Some(Trace {
                    kind: g.kind,
                    chain: chain_index as u32,
                    residues: Vec::new(),
                    guide_atoms: Vec::new(),
                    orient_atoms: Vec::new(),
                });
            }
            let t = current.as_mut().unwrap();
            t.residues.push(residue);
            t.guide_atoms.push(g.atom);
            t.orient_atoms.push(g.orient);
        }
        flush(current.take(), &mut out);
    }
    out
}
