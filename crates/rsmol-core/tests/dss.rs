use rsmol_core::io::{self, write};
use rsmol_core::{SecondaryStructure, Structure, dss};

/// SARS-CoV-2 main protease (PDB 6LU7): α and β, with struct_conf tables from the PDB's
/// DSSP-based annotation.
fn mpro() -> Structure {
    io::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/6LU7.cif.gz")).unwrap()
}

fn count(ss: &[SecondaryStructure], kind: SecondaryStructure) -> usize {
    ss.iter().filter(|&&k| k == kind).count()
}

#[test]
fn matches_the_pdb_annotation() {
    let s = mpro();
    let file: Vec<_> = s.residues.iter().map(|r| r.ss).collect();
    let computed = dss::compute(&s, 0);
    let agree = file.iter().zip(&computed).filter(|(a, b)| a == b).count();
    assert!(agree * 100 >= 98 * s.residues.len(), "{agree} of {} residues agree", s.residues.len());
    for kind in [SecondaryStructure::Helix, SecondaryStructure::Sheet] {
        assert!(count(&file, kind).abs_diff(count(&computed, kind)) <= 3, "{kind:?}");
    }
}

#[test]
fn files_without_records_get_an_assignment() {
    let mut s = mpro();
    for r in &mut s.residues {
        r.ss = SecondaryStructure::Loop;
    }
    let all: Vec<usize> = (0..s.atom_count()).collect();
    let pdb = write::to_pdb(&s, &all).unwrap();
    assert!(!pdb.contains("HELIX") && !pdb.contains("SHEET"));

    let read = io::parse_bytes(pdb.as_bytes(), "model.pdb").unwrap();
    let ss: Vec<_> = read.residues.iter().map(|r| r.ss).collect();
    assert_eq!(ss, dss::compute(&read, 0));
    assert!(count(&ss, SecondaryStructure::Helix) > 80);
    assert!(count(&ss, SecondaryStructure::Sheet) > 70);
}

#[test]
fn chain_breaks_and_partial_residues_are_loop() {
    let mut s = mpro();
    // Without carbonyl oxygens no hydrogen bond can form.
    let keep: Vec<usize> = (0..s.atom_count()).filter(|&i| s.atoms.name[i] != "O").collect();
    let pdb = write::to_pdb(&s, &keep).unwrap();
    let stripped = io::parse_bytes(pdb.as_bytes(), "no_o.pdb").unwrap();
    assert!(dss::compute(&stripped, 0).iter().all(|&k| k == SecondaryStructure::Loop));

    // Pulling one residue 50 Å away breaks the chain on both sides of it.
    let residue = s.residues.iter().position(|r| r.seq_num == 10).unwrap();
    for a in s.residues[residue].atoms.clone() {
        s.states[0][a as usize][0] += 50.0;
    }
    assert_eq!(dss::compute(&s, 0)[residue], SecondaryStructure::Loop);
}
