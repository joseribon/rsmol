//! Distance-based covalent bond detection.
//!
//! mmCIF model files only list inter-residue bonds (`struct_conn`), and PDB `CONECT` records only
//! cover ligands, so bonds within residues have to be inferred. Two atoms are bonded when they
//! are closer than the sum of their covalent radii plus [`TOLERANCE`], the rule used by Jmol and
//! similar tools. Metals are left out: their coordination bonds come from the file (`metalc`),
//! and distance alone would connect them to every nearby ligand atom.

use crate::spatial::Grid;
use crate::{Bond, BondKind, Element, Structure};

/// Slack added to the covalent radius sum, in Å.
pub const TOLERANCE: f32 = 0.45;
/// Pairs closer than this are overlapping atoms (bad models), not bonds.
const MIN_DISTANCE: f32 = 0.4;

/// Single-bond covalent radius in Å (Cordero et al. 2008), or `None` for metals and noble gases,
/// which are never bonded by distance.
pub fn covalent_radius(e: Element) -> Option<f32> {
    Some(match e.atomic_number() {
        1 => 0.31,
        5 => 0.84,
        6 => 0.76,
        7 => 0.71,
        8 => 0.66,
        9 => 0.57,
        14 => 1.11,
        15 => 1.07,
        16 => 1.05,
        17 => 1.02,
        33 => 1.19,
        34 => 1.20,
        35 => 1.20,
        52 => 1.38,
        53 => 1.39,
        // Unknown elements bond like carbon rather than not at all.
        0 => 0.76,
        _ => return None,
    })
}

/// Largest possible bond length, used as the grid search radius.
fn max_bond_length() -> f32 {
    2.0 * 1.39 + TOLERANCE
}

/// Infers covalent bonds from the coordinates of `state`.
pub fn infer(s: &Structure, state: usize) -> Vec<Bond> {
    let Some(coords) = s.coords(state) else { return Vec::new() };
    let radii: Vec<Option<f32>> = s.atoms.element.iter().map(|&e| covalent_radius(e)).collect();
    let grid = Grid::new(coords, max_bond_length());
    let mut bonds = Vec::with_capacity(coords.len() * 11 / 10);
    grid.pairs_within(coords, max_bond_length(), |i, j, d2| {
        let (a, b) = (i as usize, j as usize);
        let (Some(ra), Some(rb)) = (radii[a], radii[b]) else { return };
        let max = ra + rb + TOLERANCE;
        if d2 > max * max || d2 < MIN_DISTANCE * MIN_DISTANCE {
            return;
        }
        // Atoms in different alternate conformations never coexist.
        if let (Some(x), Some(y)) = (s.atoms.alt_loc[a], s.atoms.alt_loc[b])
            && x != y
        {
            return;
        }
        if s.atoms.element[a] == Element::H && s.atoms.element[b] == Element::H {
            return;
        }
        bonds.push(Bond { atoms: [i, j], kind: BondKind::Covalent });
    });
    bonds
}

/// Adds inferred bonds to `s.bonds`. Bonds already listed by the file keep their kind.
pub fn assign(s: &mut Structure, state: usize) {
    let mut inferred = infer(s, state);
    inferred.retain(|b| s.bonds.binary_search_by_key(&b.atoms, |x| x.atoms).is_err());
    s.bonds.extend(inferred);
    s.bonds.sort_unstable();
}
