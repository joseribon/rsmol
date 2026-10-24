use rsmol_core::{BondKind, Element, EntityKind, SecondaryStructure, Structure, io};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");

fn fixture() -> Structure {
    io::read(format!("{DATA}/two_models.cif")).unwrap()
}

#[test]
fn chains_split_by_label_id() {
    let s = fixture();
    let chains: Vec<_> = s.chains.iter().map(|c| (c.id.as_str(), c.label_id.as_str())).collect();
    assert_eq!(chains, [("A", "A"), ("B", "B"), ("B", "C"), ("B", "D")]);

    let kinds: Vec<_> = s.chains.iter().map(|c| s.entities[c.entity.unwrap() as usize].kind).collect();
    assert_eq!(kinds, [EntityKind::Polymer, EntityKind::Polymer, EntityKind::NonPolymer, EntityKind::Water]);
    assert_eq!(s.entities[1].description, "PROTOPORPHYRIN IX CONTAINING FE");
}

#[test]
fn atoms_use_author_numbering() {
    let s = fixture();
    assert_eq!(s.atom_count(), 11);
    let residues: Vec<_> = s.residues.iter().map(|r| (r.name.as_str(), r.seq_num, r.ins_code)).collect();
    assert_eq!(
        residues,
        [("GLY", 1, None), ("ALA", 2, None), ("SER", 2, Some('A')), ("GLY", 1, None), ("HEM", 201, None), ("HOH", 301, None)]
    );
    assert_eq!(s.atoms.alt_loc[5..7], [Some('A'), Some('B')]);
    assert_eq!(s.atoms.element[9], Element(26));
    assert!(s.atoms.hetero[9] && !s.atoms.hetero[8]);
}

#[test]
fn models_become_states() {
    let s = fixture();
    assert_eq!(s.state_count(), 2);
    assert_eq!(s.coords(1).unwrap()[10], [14.1, 2.0, -2.0]);
}

#[test]
fn secondary_structure() {
    let s = fixture();
    let ss: Vec<_> = s.residues.iter().map(|r| r.ss).collect();
    use SecondaryStructure::*;
    // Helix A1..A2A, turn ignored, strand B1.
    assert_eq!(ss, [Helix, Helix, Helix, Sheet, Loop, Loop]);
}

#[test]
fn struct_conn_bonds() {
    let s = fixture();
    let kind = |pair: [u32; 2]| s.bonds.iter().find(|b| b.atoms == pair).map(|b| b.kind);
    // The covalent partner without an alt id binds the first conformer.
    assert_eq!(kind([5, 7]), Some(BondKind::Covalent));
    assert_eq!(kind([8, 9]), Some(BondKind::MetalCoordination));
    // Skipped records: the bond to a symmetry mate (FE–HOH) and the hydrogen bond (O–N).
    assert_eq!(kind([9, 10]), None);
    assert_eq!(kind([3, 8]), None);
}

#[test]
fn pdb_and_mmcif_agree() {
    let pdb = io::read(format!("{DATA}/two_models.pdb")).unwrap();
    let cif = fixture();
    assert_eq!(pdb.states, cif.states);
    assert_eq!(pdb.residues.iter().map(|r| (&r.name, r.seq_num, r.ins_code)).collect::<Vec<_>>(),
               cif.residues.iter().map(|r| (&r.name, r.seq_num, r.ins_code)).collect::<Vec<_>>());
    assert_eq!(pdb.atoms.name, cif.atoms.name);
    assert_eq!(pdb.atoms.element, cif.atoms.element);
}

#[test]
fn gzipped_files_parse_like_plain_ones() {
    use std::io::Write;
    let plain = std::fs::read(format!("{DATA}/two_models.cif")).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&plain).unwrap();
    let s = io::parse_bytes(&gz.finish().unwrap(), "2MOD.cif.gz").unwrap();
    assert_eq!(s.name, "2MOD");
    assert_eq!(s.states, fixture().states);
    assert!(io::parse_bytes(b"not gzip", "x.cif.gz").unwrap_err().message.starts_with("not a valid gzip file"));
    assert_eq!(io::parse_bytes(b"", "x.xyz").unwrap_err().message, "unknown structure format '.xyz'");
}

#[test]
fn atom_labels() {
    let s = fixture();
    assert_eq!(s.atom_label(1), "A/GLY 1/CA");
    assert_eq!(s.atom_label(6), "A/ALA 2/CA`B");
    assert_eq!(s.atom_label(7), "A/SER 2A/N");
    assert_eq!(s.atom_label(9), "B/HEM 201/FE");
}

#[test]
fn concat_offsets_indices_and_pads_states() {
    let a = fixture(); // 11 atoms, 2 states
    let b = io::read(format!("{DATA}/traces.pdb")).unwrap(); // 27 atoms, 1 state
    let s = Structure::concat("scene", &[&a, &b]);
    assert_eq!(s.atom_count(), 38);
    assert_eq!(s.residues.len(), a.residues.len() + b.residues.len());
    assert_eq!(s.chain_of(11).id, b.chain_of(0).id);
    assert_eq!(s.residue_of(11).atoms.start, 11);
    assert_eq!(s.state_count(), 2);
    // traces.pdb has one state, so it repeats in state 2.
    assert_eq!(s.coords(1).unwrap()[11], b.coords(0).unwrap()[0]);
    assert!(s.bonds.iter().all(|bond| bond.atoms.iter().all(|&x| (x as usize) < 38)));
    assert!(s.bonds.iter().any(|bond| bond.atoms[0] >= 11));
}
