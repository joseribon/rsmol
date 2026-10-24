use rsmol_core::backbone::{PolymerKind, traces};
use rsmol_core::io;

#[test]
fn traces_break_at_gaps_and_skip_ions() {
    let s = io::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/traces.pdb")).unwrap();
    let t = traces(&s, 0);
    let summary: Vec<_> = t.iter().map(|t| (t.kind, t.residues.clone())).collect();
    assert_eq!(
        summary,
        [
            (PolymerKind::Protein, vec![0, 1, 2]),
            // Residue 4 follows a 10 Å gap; the calcium ion "CA" (residue index 5) is not protein.
            (PolymerKind::Protein, vec![3, 4]),
            (PolymerKind::Nucleic, vec![6, 7]),
        ]
    );
    // Guides are CA / C4'; orientation comes from the carbonyl O.
    assert_eq!(t[0].guide_atoms, [1, 5, 9]);
    assert_eq!(t[0].orient_atoms, [Some(3), Some(7), Some(11)]);
    assert_eq!(t[2].guide_atoms, [22, 25]);
    assert_eq!(t[2].orient_atoms, [None, None]);
}

#[test]
fn truncated_and_coarse_residues_still_trace() {
    // Nucleotide 2 lacks C1' (common in cryo-EM models); chain B is a CA-only protein model.
    let text = "\
ATOM      1  P     G A   1       0.000   2.000   0.000  1.00  0.00           P
ATOM      2  C4'   G A   1       0.000   0.000   0.000  1.00  0.00           C
ATOM      3  C1'   G A   1       0.000  -1.500   0.000  1.00  0.00           C
ATOM      4  C3'   G A   2       6.000   0.500   0.000  1.00  0.00           C
ATOM      5  C4'   G A   2       6.500   0.000   0.000  1.00  0.00           C
ATOM      6  CA  ALA B   1       0.000  20.000   0.000  1.00  0.00           C
ATOM      7  CA  GLY B   2       3.800  20.000   0.000  1.00  0.00           C
END
";
    let s = io::pdb::parse(text, "coarse").unwrap();
    let t = traces(&s, 0);
    let summary: Vec<_> = t.iter().map(|t| (t.kind, t.guide_atoms.clone())).collect();
    assert_eq!(summary, [(PolymerKind::Nucleic, vec![1, 4]), (PolymerKind::Protein, vec![5, 6])]);
}
