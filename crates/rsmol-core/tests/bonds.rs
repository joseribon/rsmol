use rsmol_core::{BondKind, io};

fn bonds(text: &str) -> Vec<([u32; 2], BondKind)> {
    io::pdb::parse(text, "t").unwrap().bonds.iter().map(|b| (b.atoms, b.kind)).collect()
}

#[test]
fn water_bonds_but_not_hydrogen_pairs() {
    // O–H 0.96 Å each; the two H are 1.52 Å apart, within H+H radius range but never bonded.
    let text = "\
HETATM    1  O   HOH A   1       0.000   0.000   0.000  1.00  0.00           O
HETATM    2  H1  HOH A   1       0.757   0.586   0.000  1.00  0.00           H
HETATM    3  H2  HOH A   1      -0.757   0.586   0.000  1.00  0.00           H
";
    assert_eq!(bonds(text), [([0, 1], BondKind::Covalent), ([0, 2], BondKind::Covalent)]);
}

#[test]
fn alt_locs_metals_and_overlaps_are_skipped() {
    let text = "\
ATOM      1  CA  SER A   1       0.000   0.000   0.000  1.00  0.00           C
ATOM      2  CB ASER A   1       1.530   0.000   0.000  0.50  0.00           C
ATOM      3  CB BSER A   1       1.500   0.300   0.000  0.50  0.00           C
ATOM      4  OG ASER A   1       2.000   1.350   0.000  0.50  0.00           O
HETATM    5 ZN    ZN A 101       2.000   3.300   0.000  1.00  0.00          ZN
HETATM    6  C1  LIG A 102      10.000   0.000   0.000  1.00  0.00           C
HETATM    7  C2  LIG A 102      10.100   0.000   0.000  1.00  0.00           C
";
    // CA bonds to both conformers of CB; OG(A) only to CB(A); CB(A) and CB(B) never bond each
    // other; zinc gets no distance bonds; the overlapping LIG atoms (0.1 Å) are not bonded.
    assert_eq!(
        bonds(text),
        [([0, 1], BondKind::Covalent), ([0, 2], BondKind::Covalent), ([1, 3], BondKind::Covalent)]
    );
}

#[test]
fn file_bonds_keep_their_kind() {
    let s = io::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/two_models.cif")).unwrap();
    // FE–N metal coordination from struct_conn survives alongside inferred covalent bonds.
    assert!(s.bonds.iter().any(|b| b.atoms == [8, 9] && b.kind == BondKind::MetalCoordination));
    // GLY 1 backbone: N–CA, CA–C, C–O.
    for pair in [[0, 1], [1, 2], [2, 3]] {
        assert!(s.bonds.iter().any(|b| b.atoms == pair && b.kind == BondKind::Covalent), "{pair:?}");
    }
}
