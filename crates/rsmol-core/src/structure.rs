//! Core molecular data model.
//!
//! Topology (atoms, residues, chains) is stored once; coordinates are stored per *state*
//! (PyMOL's term for NMR models / trajectory frames). Atom properties are struct-of-arrays so
//! that selections scan contiguous memory and coordinates can be handed to the GPU as-is.

use std::ops::Range;

use crate::Element;

pub type Vec3 = [f32; 3];

#[derive(Clone, Debug, Default)]
pub struct Structure {
    pub name: String,
    pub atoms: Atoms,
    pub residues: Vec<Residue>,
    pub chains: Vec<Chain>,
    pub entities: Vec<Entity>,
    /// Bonds read from the file (CONECT / struct_conn). Sorted, deduplicated, `atoms[0] < atoms[1]`.
    pub bonds: Vec<Bond>,
    /// One coordinate set per state; each has `atoms.len()` entries.
    pub states: Vec<Vec<Vec3>>,
}

/// Per-atom properties, struct-of-arrays. All vectors have the same length.
#[derive(Clone, Debug, Default)]
pub struct Atoms {
    pub name: Vec<String>,
    pub element: Vec<Element>,
    pub alt_loc: Vec<Option<char>>,
    pub b_factor: Vec<f32>,
    pub occupancy: Vec<f32>,
    pub serial: Vec<i32>,
    /// HETATM records (ligands, waters, ions).
    pub hetero: Vec<bool>,
    /// Index into `Structure::residues`.
    pub residue: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Residue {
    pub name: String,
    pub seq_num: i32,
    pub ins_code: Option<char>,
    pub ss: SecondaryStructure,
    /// Index into `Structure::chains`.
    pub chain: u32,
    pub atoms: Range<u32>,
}

/// A contiguous run of residues belonging to one molecule.
///
/// mmCIF distinguishes the author chain ID (`auth_asym_id`, what PyMOL calls `chain` and what
/// papers cite) from the per-molecule label ID (`label_asym_id`, PyMOL's `segi`): a protein chain
/// "A" and its bound ligands and waters share `id` but each gets its own `label_id`. PDB files
/// only have the author ID, so there `label_id == id`.
#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    pub id: String,
    pub label_id: String,
    /// Index into `Structure::entities`.
    pub entity: Option<u32>,
    pub residues: Range<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub id: String,
    pub kind: EntityKind,
    pub description: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EntityKind {
    Polymer,
    NonPolymer,
    Water,
    Branched,
    #[default]
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SecondaryStructure {
    #[default]
    Loop,
    Helix,
    Sheet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Bond {
    pub atoms: [u32; 2],
    pub kind: BondKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BondKind {
    Covalent,
    Disulfide,
    MetalCoordination,
}

impl Atoms {
    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

impl Structure {
    pub fn atom_count(&self) -> usize {
        self.atoms.len()
    }

    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    /// Coordinates of `state` (0-based).
    pub fn coords(&self, state: usize) -> Option<&[Vec3]> {
        self.states.get(state).map(Vec::as_slice)
    }

    pub fn residue_of(&self, atom: usize) -> &Residue {
        &self.residues[self.atoms.residue[atom] as usize]
    }

    pub fn chain_of(&self, atom: usize) -> &Chain {
        &self.chains[self.residue_of(atom).chain as usize]
    }

    /// Axis-aligned bounding box of a state, or `None` if it has no atoms.
    pub fn bounds(&self, state: usize) -> Option<(Vec3, Vec3)> {
        let coords = self.coords(state)?;
        let first = *coords.first()?;
        Some(coords.iter().fold((first, first), |(mut lo, mut hi), p| {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
            (lo, hi)
        }))
    }
}
