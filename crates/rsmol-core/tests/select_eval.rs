use std::collections::HashMap;

use rsmol_core::select::{AtomSet, Evaluator, parse, select};
use rsmol_core::{Structure, io};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");

fn load(name: &str) -> Structure {
    io::read(format!("{DATA}/{name}")).unwrap()
}

fn atoms(s: &Structure, text: &str) -> Vec<usize> {
    match select(s, text) {
        Ok(set) => set.iter().collect(),
        Err(e) => panic!("{}", e.render(text)),
    }
}

// two_models.cif atoms:
//  0 N   GLY A1     4 N   ALA A2       8 N  GLY B1
//  1 CA  GLY A1     5 CA  ALA A2 alt A 9 FE HEM B201 (segi C)
//  2 C   GLY A1     6 CA  ALA A2 alt B 10 O HOH B301 (segi D)
//  3 O   GLY A1     7 N   SER A2A

#[test]
fn names_and_identifiers() {
    let s = load("two_models.cif");
    assert_eq!(atoms(&s, "name CA"), [1, 5, 6]);
    assert_eq!(atoms(&s, "name c*"), [1, 2, 5, 6]);
    assert_eq!(atoms(&s, "resn hem"), [9]);
    assert_eq!(atoms(&s, "chain B"), [8, 9, 10]);
    // Chain IDs are case-sensitive, like PyMOL's default.
    assert_eq!(atoms(&s, "chain b"), Vec::<usize>::new());
    assert_eq!(atoms(&s, "segi C+D"), [9, 10]);
    assert_eq!(atoms(&s, "e. fe"), [9]);
    assert_eq!(atoms(&s, "alt A"), [5]);
    assert_eq!(atoms(&s, "alt ''").len(), 9);
    assert_eq!(atoms(&s, "index 1-3"), [0, 1, 2]);
    assert_eq!(atoms(&s, "id 11"), [10]);
    assert_eq!(atoms(&s, "model two_models"), (0..11).collect::<Vec<_>>());
}

#[test]
fn residue_numbers_with_insertion_codes() {
    let s = load("two_models.cif");
    assert_eq!(atoms(&s, "resi 2"), [4, 5, 6]);
    assert_eq!(atoms(&s, "resi 2A"), [7]);
    assert_eq!(atoms(&s, "resi 1-2 and chain A"), [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(atoms(&s, "resi 2-2A"), [4, 5, 6, 7]);
    assert_eq!(atoms(&s, "resi 201+301"), [9, 10]);
}

#[test]
fn properties_classes_and_states() {
    let s = load("two_models.cif");
    assert_eq!(atoms(&s, "b > 35"), [10]);
    assert_eq!(atoms(&s, "q < 1"), [4, 5, 6]);
    assert_eq!(atoms(&s, "hetatm"), [9, 10]);
    assert_eq!(atoms(&s, "solvent"), [10]);
    assert_eq!(atoms(&s, "metals"), [9]);
    assert_eq!(atoms(&s, "state 2").len(), 11);
    assert_eq!(atoms(&s, "state 3"), Vec::<usize>::new());
    assert_eq!(atoms(&s, "first chain B or last chain A"), [7, 8]);
}

// traces.pdb atoms: residues A1–A5 (ALA, N CA C O each) = 0..20, with a 10 Å gap between
// A3 and A4; 20 = calcium ion "CA" next to A5; 21–26 = two nucleotides (P, C4', C1').

#[test]
fn polymer_classes_and_backbone() {
    let s = load("traces.pdb");
    assert_eq!(atoms(&s, "polymer.protein"), (0..20).collect::<Vec<_>>());
    assert_eq!(atoms(&s, "polymer.nucleic"), [21, 22, 23, 24, 25, 26]);
    assert_eq!(atoms(&s, "name CA and not polymer"), [20]);
    assert_eq!(atoms(&s, "inorganic"), [20]);
    assert_eq!(atoms(&s, "organic"), Vec::<usize>::new());
    assert_eq!(atoms(&s, "bb. and chain B"), [21, 22, 24, 25]);
    assert_eq!(atoms(&s, "sc."), [23, 26]);
}

#[test]
fn distance_operators() {
    let s = load("traces.pdb");
    // The calcium sits 2.45 Å from C and 2.94 Å from O of A5, 3.6 Å from its CA.
    assert_eq!(atoms(&s, "inorganic around 3"), [18, 19]);
    assert_eq!(atoms(&s, "inorganic expand 3"), [18, 19, 20]);
    assert_eq!(atoms(&s, "polymer within 3 of inorganic"), [18, 19]);
    assert_eq!(atoms(&s, "all near_to 3 of inorganic"), [18, 19]);
    assert_eq!(atoms(&s, "polymer beyond 3 of inorganic").len(), 26 - 2);
    // Prefix operators bind tighter than `around`, so the grouping needs parentheses.
    assert_eq!(atoms(&s, "byres (inorganic around 3)"), [16, 17, 18, 19]);
    assert_eq!(atoms(&s, "byres inorganic around 3"), [18, 19]);
}

#[test]
fn bond_operators() {
    let s = load("traces.pdb");
    assert_eq!(atoms(&s, "neighbor (name CA and resi 1)"), [0, 2]);
    assert_eq!(atoms(&s, "bto. (name CA and resi 1)"), [0, 1, 2]);
    // C of A1 is bonded to N of A2, so two bonds out from CA1 reaches into A2.
    assert_eq!(atoms(&s, "(name CA and resi 1) extend 2"), [0, 1, 2, 3, 4]);
    // A1–A3 are one covalent molecule; A4–A5 are separate after the gap.
    assert_eq!(atoms(&s, "bymolecule (resi 1 and chain A)"), (0..12).collect::<Vec<_>>());
    // Chain A includes the calcium ion; chain B is the nucleotide pair.
    assert_eq!(atoms(&s, "bychain resi 1").len(), s.atom_count());
    assert_eq!(atoms(&s, "bychain (resi 1 and polymer.nucleic)"), (21..27).collect::<Vec<_>>());
}

#[test]
fn named_selections() {
    let s = load("traces.pdb");
    let text = "sele and name CA";
    let err = select(&s, text).unwrap_err();
    assert_eq!(err.span, 0..4);
    assert_eq!(err.message, "no selection or object named 'sele'");

    let mut named = HashMap::new();
    named.insert("sele".to_owned(), AtomSet::from_indices(s.atom_count(), 0..8));
    let set = Evaluator::new(&s, 0).with_named(&named).eval(&parse(text).unwrap()).unwrap();
    assert_eq!(set.iter().collect::<Vec<_>>(), [1, 5]);
}
