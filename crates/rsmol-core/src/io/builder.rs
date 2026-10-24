//! Format-independent structure assembly shared by the PDB and mmCIF readers, so both group
//! atoms into residues and chains the same way.

use std::collections::HashMap;

use crate::structure::{
    Bond, BondKind, Chain, Entity, EntityKind, Residue, SecondaryStructure, Structure, Vec3,
};
use crate::Element;

/// One ATOM/HETATM record, borrowed from the input text.
pub(crate) struct AtomRecord<'a> {
    pub pos: Vec3,
    pub name: &'a str,
    pub res_name: &'a str,
    /// Author chain ID.
    pub chain_id: &'a str,
    /// Label chain ID; equal to `chain_id` for PDB files.
    pub label_chain_id: &'a str,
    pub entity_id: Option<&'a str>,
    pub seq_num: i32,
    pub ins_code: Option<char>,
    pub alt_loc: Option<char>,
    pub element: Element,
    pub occupancy: f32,
    pub b_factor: f32,
    pub serial: i32,
    pub hetero: bool,
}

/// A HELIX/SHEET range in author numbering, inclusive at both ends.
pub(crate) struct SsRange {
    pub kind: SecondaryStructure,
    pub chain_id: String,
    pub start: (i32, Option<char>),
    pub end: (i32, Option<char>),
}

pub(crate) struct StructureBuilder {
    s: Structure,
    coords: Vec<Vec3>,
    /// `true` once the first state is complete; later states only contribute coordinates.
    topology_done: bool,
    entity_index: HashMap<String, u32>,
    ss_ranges: Vec<SsRange>,
}

impl StructureBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        StructureBuilder {
            s: Structure { name: name.into(), ..Default::default() },
            coords: Vec::new(),
            topology_done: false,
            entity_index: HashMap::new(),
            ss_ranges: Vec::new(),
        }
    }

    pub fn add_entity(&mut self, id: &str, kind: EntityKind, description: &str) {
        self.entity_index.insert(id.to_owned(), self.s.entities.len() as u32);
        self.s.entities.push(Entity { id: id.to_owned(), kind, description: description.to_owned() });
    }

    pub fn add_secondary_structure(&mut self, range: SsRange) {
        self.ss_ranges.push(range);
    }

    pub fn add_bond(&mut self, a: u32, b: u32, kind: BondKind) {
        if a != b {
            self.s.bonds.push(Bond { atoms: [a.min(b), a.max(b)], kind });
        }
    }

    pub fn atom_count(&self) -> usize {
        self.s.atom_count()
    }

    pub fn atom(&mut self, rec: AtomRecord) -> Result<(), String> {
        if self.topology_done {
            if self.coords.len() >= self.s.atom_count() {
                return Err("state has more atoms than state 1".into());
            }
        } else {
            self.push_topology(&rec);
        }
        self.coords.push(rec.pos);
        Ok(())
    }

    /// Closes the current state (MODEL). Empty states are ignored.
    pub fn end_state(&mut self) -> Result<(), String> {
        if self.coords.is_empty() {
            return Ok(());
        }
        if self.coords.len() != self.s.atom_count() {
            return Err(format!(
                "state has {} atoms, state 1 has {}",
                self.coords.len(),
                self.s.atom_count()
            ));
        }
        self.s.states.push(std::mem::take(&mut self.coords));
        self.topology_done = true;
        Ok(())
    }

    pub fn finish(mut self) -> Result<Structure, String> {
        self.end_state()?;
        self.assign_secondary_structure();
        self.s.bonds.sort_unstable();
        self.s.bonds.dedup_by_key(|b| b.atoms);
        crate::bonds::assign(&mut self.s, 0);
        Ok(self.s)
    }

    fn push_topology(&mut self, rec: &AtomRecord) {
        let s = &mut self.s;
        let atom_index = s.atoms.len() as u32;

        let chain_changed = s
            .chains
            .last()
            .is_none_or(|c| c.id != rec.chain_id || c.label_id != rec.label_chain_id);
        if chain_changed {
            let start = s.residues.len() as u32;
            s.chains.push(Chain {
                id: rec.chain_id.to_owned(),
                label_id: rec.label_chain_id.to_owned(),
                entity: rec.entity_id.and_then(|id| self.entity_index.get(id).copied()),
                residues: start..start,
            });
        }
        let chain = (s.chains.len() - 1) as u32;

        let residue_changed = chain_changed
            || s.residues.last().is_none_or(|r| {
                r.seq_num != rec.seq_num || r.ins_code != rec.ins_code || r.name != rec.res_name
            });
        if residue_changed {
            s.residues.push(Residue {
                name: rec.res_name.to_owned(),
                seq_num: rec.seq_num,
                ins_code: rec.ins_code,
                ss: SecondaryStructure::Loop,
                chain,
                atoms: atom_index..atom_index,
            });
            s.chains.last_mut().unwrap().residues.end += 1;
        }
        s.residues.last_mut().unwrap().atoms.end += 1;

        let a = &mut s.atoms;
        a.name.push(rec.name.to_owned());
        a.element.push(rec.element);
        a.alt_loc.push(rec.alt_loc);
        a.occupancy.push(rec.occupancy);
        a.b_factor.push(rec.b_factor);
        a.serial.push(rec.serial);
        a.hetero.push(rec.hetero);
        a.residue.push((s.residues.len() - 1) as u32);
    }

    fn assign_secondary_structure(&mut self) {
        if self.ss_ranges.is_empty() {
            return;
        }
        let s = &mut self.s;
        // First occurrence wins: polymer residues precede ligands/waters that may reuse numbers.
        let mut index: HashMap<(&str, i32, Option<char>), usize> = HashMap::new();
        for (i, r) in s.residues.iter().enumerate() {
            let chain = s.chains[r.chain as usize].id.as_str();
            index.entry((chain, r.seq_num, r.ins_code)).or_insert(i);
        }
        let mut assignments = Vec::new();
        for range in &self.ss_ranges {
            let chain = range.chain_id.as_str();
            let (Some(&start), Some(&end)) = (
                index.get(&(chain, range.start.0, range.start.1)),
                index.get(&(chain, range.end.0, range.end.1)),
            ) else {
                continue;
            };
            if start <= end && s.residues[start].chain == s.residues[end].chain {
                assignments.push((start..=end, range.kind));
            }
        }
        for (range, kind) in assignments {
            for r in &mut s.residues[range] {
                r.ss = kind;
            }
        }
    }
}
