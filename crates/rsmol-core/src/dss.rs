//! Secondary structure from backbone geometry, following DSSP's hydrogen-bond rules
//! (Kabsch & Sander, 1983).
//!
//! Files from the PDB carry their assignment (HELIX/SHEET records, struct_conf tables), but
//! predicted models (AlphaFold, ESMFold, Boltz, …) usually have coordinates only. For those the
//! readers call [`assign`], as PyMOL runs `dss` on load.
//!
//! Backbone hydrogen bonds are scored with DSSP's electrostatic energy, keeping each NH's two
//! best acceptors. Two consecutive i→i+n turns make a helix (α, π, and 3₁₀ where nothing else
//! claims the residues); bridges between strands, joined into ladders and across β-bulges, make
//! sheets. Helix types are not told apart, as PDB files list 3₁₀ helices as helices too, and
//! isolated bridges, turns and bends are loop: that is all the cartoon distinguishes.

use crate::spatial::Grid;
use crate::{SecondaryStructure, Structure, Vec3};

/// DSSP's hydrogen-bond cutoff, kcal/mol.
const MAX_HBOND_ENERGY: f32 = -0.5;
/// Residues whose CAs are farther apart than this (Å) cannot be hydrogen-bonded.
const MAX_CA_DISTANCE: f32 = 9.0;
/// A peptide C–N longer than this (Å) is a chain break.
const MAX_PEPTIDE_BOND: f32 = 2.5;

/// Assigns secondary structure to every residue of `s` from the coordinates of `state`.
pub fn assign(s: &mut Structure, state: usize) {
    let assignment = compute(s, state);
    for (residue, ss) in s.residues.iter_mut().zip(assignment) {
        residue.ss = ss;
    }
}

/// The assignment for each residue of `s`, in residue order. Residues without a full protein
/// backbone (N, CA, C, O) are loop.
pub fn compute(s: &Structure, state: usize) -> Vec<SecondaryStructure> {
    let mut out = vec![SecondaryStructure::Loop; s.residues.len()];
    let Some(coords) = s.coords(state) else { return out };
    let bb = backbone(s, coords);
    if bb.len() < 5 {
        return out;
    }
    let hb = HBonds::new(&bb);
    let linked = |a: usize, b: usize| b < bb.len() && bb[a].segment == bb[b].segment;

    // Helices: consecutive n-turns at i-1 and i make i..i+n helical. α (n = 4) and π (n = 5)
    // helices take any residue; 3₁₀ helices (n = 3) only fill residues still free, as in DSSP.
    let turn = |i: usize, n: usize| linked(i, i + n) && hb.bonded(i, i + n);
    let mut helix = vec![false; bb.len()];
    for n in [4, 5] {
        for i in 1..bb.len() {
            if turn(i - 1, n) && turn(i, n) {
                helix[i..i + n].fill(true);
            }
        }
    }

    // Bridges between residues i < j; their neighbors must exist without chain breaks.
    let mut bridges = Vec::new();
    for &(i, j) in &hb.pairs {
        let (i, j) = (i as usize, j as usize);
        if j < i + 3 || i == 0 || !linked(i - 1, i + 1) || !linked(j - 1, j + 1) {
            continue;
        }
        let parallel = (hb.bonded(i - 1, j) && hb.bonded(j, i + 1)) || (hb.bonded(j - 1, i) && hb.bonded(i, j + 1));
        let antiparallel = (hb.bonded(i, j) && hb.bonded(j, i)) || (hb.bonded(i - 1, j + 1) && hb.bonded(j - 1, i + 1));
        if parallel || antiparallel {
            bridges.push(Bridge { i, j, parallel });
        }
    }
    bridges.sort_unstable_by_key(|b| (b.i, b.j));

    // Ladders: runs of bridges of one kind, i stepping by one and j by ±1.
    let mut ladders: Vec<Ladder> = Vec::new();
    for b in bridges {
        let extends = ladders.iter_mut().rev().find(|l| {
            l.parallel == b.parallel
                && l.i.1 + 1 == b.i
                && linked(l.i.1, b.i)
                && if b.parallel { l.j.1 + 1 == b.j && linked(l.j.1, b.j) } else { l.j.0 == b.j + 1 && linked(b.j, l.j.0) }
        });
        match extends {
            Some(l) => {
                l.i.1 = b.i;
                if b.parallel { l.j.1 = b.j } else { l.j.0 = b.j }
                l.bridges += 1;
            }
            None => ladders.push(Ladder { parallel: b.parallel, i: (b.i, b.i), j: (b.j, b.j), bridges: 1 }),
        }
    }

    // Strands: ladders of two or more bridges, and ladders joined across a β-bulge (a gap of
    // at most one residue on one strand and four on the other). A lone bridge stays loop.
    let mut strand = vec![false; bb.len()];
    let mut three_ten = vec![false; bb.len()];
    let mut mark = |(lo, hi): (usize, usize)| strand[lo..=hi].fill(true);
    for (k, a) in ladders.iter().enumerate() {
        if a.bridges > 1 {
            mark(a.i);
            mark(a.j);
        }
        for b in &ladders[k + 1..] {
            if b.parallel != a.parallel || b.i.0 <= a.i.1 || !linked(a.i.0, b.i.1) {
                continue;
            }
            let gap_i = b.i.0 - a.i.1;
            let (gap_j, j) = if a.parallel {
                (b.j.0 as isize - a.j.1 as isize, (a.j.0, b.j.1))
            } else {
                (a.j.0 as isize - b.j.1 as isize, (b.j.0, a.j.1))
            };
            let bulge = gap_j >= 1 && ((gap_i < 6 && gap_j < 3) || (gap_i < 3 && gap_j < 6));
            if bulge && linked(j.0, j.1) {
                mark((a.i.0, b.i.1));
                mark(j);
            }
        }
    }

    for i in 1..bb.len() {
        if turn(i - 1, 3) && turn(i, 3) && (i..i + 3).all(|k| !helix[k] && !strand[k] || three_ten[k]) {
            three_ten[i..i + 3].fill(true);
        }
    }

    let mut ss: Vec<SecondaryStructure> = (0..bb.len())
        .map(|k| match (helix[k], strand[k], three_ten[k]) {
            (true, _, _) => SecondaryStructure::Helix,
            (_, true, _) => SecondaryStructure::Sheet,
            (_, _, true) => SecondaryStructure::Helix,
            _ => SecondaryStructure::Loop,
        })
        .collect();
    // DSSP leaves out the residue before a helix and the one after, whose CO and NH make its
    // first and last hydrogen bonds; PDB HELIX records include them, and so do we.
    let is_helix: Vec<bool> = ss.iter().map(|&kind| kind == SecondaryStructure::Helix).collect();
    for k in 0..bb.len() {
        let starts = k + 1 < bb.len() && is_helix[k + 1] && linked(k, k + 1);
        let ends = k > 0 && is_helix[k - 1] && linked(k - 1, k);
        if ss[k] == SecondaryStructure::Loop && (starts || ends) {
            ss[k] = SecondaryStructure::Helix;
        }
    }
    for (r, kind) in bb.iter().zip(ss) {
        out[r.residue] = kind;
    }
    out
}

struct Residue {
    /// Index into `Structure::residues`.
    residue: usize,
    /// Consecutive residues share a segment unless a chain ends or the peptide bond is broken.
    segment: usize,
    n: Vec3,
    ca: Vec3,
    c: Vec3,
    o: Vec3,
    /// The amide hydrogen, placed opposite the previous carbonyl; none at a segment start or
    /// for proline.
    h: Option<Vec3>,
}

/// Protein residues with complete backbones, in residue order. The first alternate location
/// of each atom is used.
fn backbone(s: &Structure, coords: &[Vec3]) -> Vec<Residue> {
    let mut out: Vec<Residue> = Vec::new();
    for (index, r) in s.residues.iter().enumerate() {
        let find = |name: &str| {
            (r.atoms.start..r.atoms.end).find(|&a| s.atoms.name[a as usize] == name).map(|a| coords[a as usize])
        };
        let (Some(n), Some(ca), Some(c), Some(o)) = (find("N"), find("CA"), find("C"), find("O")) else { continue };
        let previous = out.last().filter(|p| s.residues[p.residue].chain == r.chain && dist(p.c, n) <= MAX_PEPTIDE_BOND);
        let segment = match (previous, out.last()) {
            (Some(p), _) => p.segment,
            (None, Some(p)) => p.segment + 1,
            (None, None) => 0,
        };
        let h = previous.filter(|_| r.name != "PRO").map(|p| {
            let d = sub(p.c, p.o);
            let len = dist(p.c, p.o).max(1e-6);
            [n[0] + d[0] / len, n[1] + d[1] / len, n[2] + d[2] / len]
        });
        out.push(Residue { residue: index, segment, n, ca, c, o, h });
    }
    out
}

/// Backbone hydrogen bonds: the two lowest-energy acceptors of each NH, as in DSSP.
struct HBonds {
    /// `acceptors[donor]`: up to two (acceptor, energy) pairs.
    acceptors: Vec<[(u32, f32); 2]>,
    /// Residue pairs `i < j` with CAs in hydrogen-bonding range.
    pairs: Vec<(u32, u32)>,
}

impl HBonds {
    fn new(bb: &[Residue]) -> HBonds {
        let none = (u32::MAX, 0.0);
        let mut hb = HBonds { acceptors: vec![[none; 2]; bb.len()], pairs: Vec::new() };
        let cas: Vec<Vec3> = bb.iter().map(|r| r.ca).collect();
        let mut pairs = Vec::new();
        Grid::new(&cas, MAX_CA_DISTANCE).pairs_within(&cas, MAX_CA_DISTANCE, |i, j, _| pairs.push((i, j)));
        for &(i, j) in &pairs {
            hb.consider(bb, j as usize, i as usize);
            hb.consider(bb, i as usize, j as usize);
        }
        hb.pairs = pairs;
        hb
    }

    /// Scores the bond from `donor`'s NH to `acceptor`'s CO and keeps it if it is among the
    /// donor's two best.
    fn consider(&mut self, bb: &[Residue], donor: usize, acceptor: usize) {
        if donor.abs_diff(acceptor) < 2 {
            return;
        }
        let Some(h) = bb[donor].h else { return };
        let energy = energy(bb[donor].n, h, bb[acceptor].c, bb[acceptor].o);
        if energy >= MAX_HBOND_ENERGY {
            return;
        }
        let best = &mut self.acceptors[donor];
        let entry = (acceptor as u32, energy);
        if best[0].0 == u32::MAX || energy < best[0].1 {
            best[1] = best[0];
            best[0] = entry;
        } else if best[1].0 == u32::MAX || energy < best[1].1 {
            best[1] = entry;
        }
    }

    /// Whether residue `co`'s carbonyl accepts a hydrogen bond from residue `nh`'s amide.
    fn bonded(&self, co: usize, nh: usize) -> bool {
        self.acceptors.get(nh).is_some_and(|best| best.iter().any(|&(a, _)| a as usize == co))
    }
}

/// DSSP's electrostatic hydrogen-bond energy, kcal/mol.
fn energy(n: Vec3, h: Vec3, c: Vec3, o: Vec3) -> f32 {
    let (on, ch, oh, cn) = (dist(o, n), dist(c, h), dist(o, h), dist(c, n));
    if on.min(ch).min(oh).min(cn) < 0.5 {
        return -9.9;
    }
    0.084 * 332.0 * (1.0 / on + 1.0 / ch - 1.0 / oh - 1.0 / cn)
}

struct Bridge {
    i: usize,
    j: usize,
    parallel: bool,
}

/// Consecutive bridges; `i` and `j` are the inclusive residue ranges on each strand.
struct Ladder {
    parallel: bool,
    i: (usize, usize),
    j: (usize, usize),
    bridges: usize,
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dist(a: Vec3, b: Vec3) -> f32 {
    let d = sub(a, b);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}
