use rsmol_core::{Element, io::pdb};

fn fixture() -> rsmol_core::Structure {
    pdb::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/two_models.pdb")).unwrap()
}

#[test]
fn topology() {
    let s = fixture();
    assert_eq!(s.name, "two_models");
    assert_eq!(s.atom_count(), 11);
    assert_eq!(s.chains.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["A", "B"]);

    let residues: Vec<_> = s.residues.iter().map(|r| (r.name.as_str(), r.seq_num, r.ins_code)).collect();
    assert_eq!(
        residues,
        [
            ("GLY", 1, None),
            ("ALA", 2, None),
            ("SER", 2, Some('A')),
            ("GLY", 1, None),
            ("HEM", 201, None),
            ("HOH", 301, None),
        ]
    );
    assert_eq!(s.residues[1].atoms, 4..7);
    assert_eq!(s.chains[1].residues, 3..6);
    assert_eq!(s.chain_of(9).id, "B");
}

#[test]
fn atom_properties() {
    let s = fixture();
    let a = &s.atoms;
    assert_eq!(a.alt_loc[5..7], [Some('A'), Some('B')]);
    assert_eq!(a.element[9], Element(26));
    assert!(a.hetero[9] && a.hetero[10] && !a.hetero[8]);
    assert_eq!(a.occupancy[4], 0.5);
    assert_eq!(a.b_factor[10], 40.0);
    assert_eq!(s.residue_of(9).name, "HEM");
}

#[test]
fn states() {
    let s = fixture();
    assert_eq!(s.state_count(), 2);
    assert_eq!(s.coords(0).unwrap()[0], [0.0, 0.0, 0.0]);
    assert_eq!(s.coords(1).unwrap()[0], [0.1, 0.0, 0.0]);
    let (lo, hi) = s.bounds(0).unwrap();
    assert_eq!(lo, [0.0, 0.0, -2.0]);
    assert_eq!(hi, [14.0, 3.0, 0.1]);
}

#[test]
fn mismatched_models_are_rejected() {
    let text = "\
MODEL        1
ATOM      1  N   GLY A   1       0.000   0.000   0.000  1.00 10.00           N
ATOM      2  CA  GLY A   1       1.458   0.000   0.000  1.00 11.00           C
ENDMDL
MODEL        2
ATOM      1  N   GLY A   1       0.000   0.000   0.000  1.00 10.00           N
ENDMDL
";
    let err = pdb::parse(text, "bad").unwrap_err();
    assert_eq!(err.line, Some(7));
}
