//! Per-atom display state: which representations each atom is shown in, and its color.
//!
//! This is PyMOL's model: `show sticks, sele` sets a flag on those atoms rather than switching a
//! global mode. A cartoon is drawn for residues whose backbone guide atom has `CARTOON` set.

use std::collections::HashMap;
use std::ops::{BitOr, BitOrAssign};

use crate::color::{self, Rgba};
use crate::select::{AtomSet, Evaluator, parse};
use crate::{Element, Structure};

/// A set of representations, as bit flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Reps(u8);

impl Reps {
    pub const NONE: Reps = Reps(0);
    pub const SPHERES: Reps = Reps(1);
    pub const STICKS: Reps = Reps(2);
    pub const CARTOON: Reps = Reps(4);
    pub const SURFACE: Reps = Reps(8);
    pub const ALL: Reps = Reps(15);

    pub fn bits(self) -> u8 {
        self.0
    }

    /// Unknown bits (from a newer file format) are dropped.
    pub fn from_bits(bits: u8) -> Reps {
        Reps(bits & Reps::ALL.0)
    }

    pub fn contains(self, other: Reps) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn intersects(self, other: Reps) -> bool {
        self.0 & other.0 != 0
    }

    pub fn without(self, other: Reps) -> Reps {
        Reps(self.0 & !other.0)
    }

    /// Representation by name, accepting PyMOL's singular/plural spellings.
    pub fn from_name(name: &str) -> Option<Reps> {
        Some(match name.to_ascii_lowercase().as_str() {
            "spheres" | "sphere" | "vdw" => Reps::SPHERES,
            "sticks" | "stick" | "licorice" => Reps::STICKS,
            "cartoon" | "cartoons" | "ribbon" => Reps::CARTOON,
            "surface" | "surfaces" | "surf" => Reps::SURFACE,
            "everything" | "all" => Reps::ALL,
            _ => return None,
        })
    }
}

impl BitOr for Reps {
    type Output = Reps;
    fn bitor(self, rhs: Reps) -> Reps {
        Reps(self.0 | rhs.0)
    }
}

impl BitOrAssign for Reps {
    fn bitor_assign(&mut self, rhs: Reps) {
        self.0 |= rhs.0;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub reps: Vec<Reps>,
    pub colors: Vec<Rgba>,
}

impl Display {
    /// The default look: polymers as cartoon, ligands and ions as sticks, water hidden. Carbons
    /// take their chain color (light gray for ligands); other elements their element color.
    /// Structures without polymers show every non-water atom as sticks.
    pub fn default_for(s: &Structure) -> Display {
        let n = s.atom_count();
        let class = |text: &str| Evaluator::new(s, 0).eval(&parse(text).unwrap()).unwrap();
        let polymer = class("polymer");
        let ligands = class("not polymer and not solvent");
        let mut reps = vec![Reps::NONE; n];
        if polymer.is_empty() {
            for i in ligands.iter() {
                reps[i] = Reps::STICKS;
            }
        } else {
            for i in polymer.iter() {
                reps[i] = Reps::CARTOON;
            }
            for i in ligands.iter() {
                reps[i] = Reps::STICKS;
            }
        }
        let colors = default_colors(s, &ligands);
        Display { reps, colors }
    }

    pub fn show(&mut self, reps: Reps, atoms: &AtomSet) {
        for i in atoms.iter() {
            self.reps[i] |= reps;
        }
    }

    pub fn hide(&mut self, reps: Reps, atoms: &AtomSet) {
        for i in atoms.iter() {
            self.reps[i] = self.reps[i].without(reps);
        }
    }

    /// PyMOL's `as`: show only `reps` for these atoms.
    pub fn show_only(&mut self, reps: Reps, atoms: &AtomSet) {
        for i in atoms.iter() {
            self.reps[i] = reps;
        }
    }

    pub fn color(&mut self, atoms: &AtomSet, mut color: impl FnMut(usize) -> Rgba) {
        for i in atoms.iter() {
            self.colors[i] = color(i);
        }
    }

    /// Atoms shown in at least one representation, for the `visible` keyword.
    pub fn visible(&self) -> AtomSet {
        AtomSet::from_fn(self.reps.len(), |i| self.reps[i] != Reps::NONE)
    }
}

/// Chain colors for carbons, keyed by author chain ID so ligand segments match their chain.
pub fn chain_colors(s: &Structure) -> Vec<Rgba> {
    let mut index: HashMap<&str, usize> = HashMap::new();
    s.chains
        .iter()
        .map(|c| {
            let next = index.len();
            color::chain(*index.entry(c.id.as_str()).or_insert(next))
        })
        .collect()
}

fn default_colors(s: &Structure, ligands: &AtomSet) -> Vec<Rgba> {
    let chains = chain_colors(s);
    (0..s.atom_count())
        .map(|i| {
            color::element(s.atoms.element[i]).unwrap_or(if ligands.contains(i) {
                color::LIGAND_CARBON
            } else {
                chains[s.residue_of(i).chain as usize]
            })
        })
        .collect()
}

/// Whether atom `i` keeps its element color under `byelement` (everything but carbon).
pub fn is_heteroatom(s: &Structure, i: usize) -> bool {
    s.atoms.element[i] != Element::C && s.atoms.element[i] != Element::UNKNOWN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rep_names_and_flags() {
        assert_eq!(Reps::from_name("Stick"), Some(Reps::STICKS));
        assert_eq!(Reps::from_name("everything"), Some(Reps::ALL));
        assert_eq!(Reps::from_name("lines"), None);
        let r = Reps::CARTOON | Reps::STICKS;
        assert!(r.contains(Reps::STICKS) && !r.contains(Reps::SPHERES));
        assert_eq!(r.without(Reps::CARTOON), Reps::STICKS);
    }
}
