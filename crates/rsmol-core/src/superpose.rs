//! Structural superposition, in the spirit of PyMOL's `align`.
//!
//! Residues are paired by a global sequence alignment (Needleman–Wunsch, BLOSUM62 for proteins),
//! the paired guide atoms (CA / C4') are fitted with Horn's quaternion method, and pairs that
//! deviate by more than `REJECT` × RMSD are dropped for up to `CYCLES` refits.

use crate::Vec3;

const CYCLES: usize = 5;
const REJECT: f64 = 2.0;
const GAP: i32 = -4;

/// A rigid transform `p' = R p + t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub rotation: [[f32; 3]; 3],
    pub translation: [f32; 3],
}

impl Transform {
    pub const IDENTITY: Transform =
        Transform { rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], translation: [0.0; 3] };

    pub fn apply(&self, p: Vec3) -> Vec3 {
        let r = &self.rotation;
        std::array::from_fn(|i| r[i][0] * p[0] + r[i][1] * p[1] + r[i][2] * p[2] + self.translation[i])
    }

    /// `self` after `first`: applying the result equals applying `first`, then `self`.
    pub fn after(&self, first: &Transform) -> Transform {
        let (a, b) = (&self.rotation, &first.rotation);
        let rotation = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()));
        let moved = Transform { rotation: self.rotation, translation: [0.0; 3] }.apply(first.translation);
        let translation = std::array::from_fn(|i| moved[i] + self.translation[i]);
        Transform { rotation, translation }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignResult {
    /// Moves the mobile atoms onto the target.
    pub transform: Transform,
    /// Over the pairs kept after outlier rejection.
    pub rmsd: f32,
    pub aligned: usize,
    /// Pairs from the sequence alignment, before rejection.
    pub paired: usize,
    pub cycles: usize,
}

/// One-letter code for sequence alignment; `X` for anything unknown.
pub fn one_letter(residue: &str) -> u8 {
    match residue {
        "ALA" => b'A', "ARG" => b'R', "ASN" => b'N', "ASP" => b'D', "CYS" => b'C',
        "GLN" => b'Q', "GLU" => b'E', "GLY" => b'G', "HIS" => b'H', "ILE" => b'I',
        "LEU" => b'L', "LYS" => b'K', "MET" | "MSE" => b'M', "PHE" => b'F', "PRO" => b'P',
        "SER" => b'S', "THR" => b'T', "TRP" => b'W', "TYR" => b'Y', "VAL" => b'V',
        // Nucleotides use lowercase so they never pair with amino acids.
        "A" | "DA" => b'a', "C" | "DC" => b'c', "G" | "DG" => b'g', "U" | "DU" => b'u', "DT" => b't',
        _ => b'X',
    }
}

const BLOSUM_ORDER: &[u8; 20] = b"ARNDCQEGHILKMFPSTWYV";
#[rustfmt::skip]
const BLOSUM62: [[i8; 20]; 20] = [
    [ 4,-1,-2,-2, 0,-1,-1, 0,-2,-1,-1,-1,-1,-2,-1, 1, 0,-3,-2, 0],
    [-1, 5, 0,-2,-3, 1, 0,-2, 0,-3,-2, 2,-1,-3,-2,-1,-1,-3,-2,-3],
    [-2, 0, 6, 1,-3, 0, 0, 0, 1,-3,-3, 0,-2,-3,-2, 1, 0,-4,-2,-3],
    [-2,-2, 1, 6,-3, 0, 2,-1,-1,-3,-4,-1,-3,-3,-1, 0,-1,-4,-3,-3],
    [ 0,-3,-3,-3, 9,-3,-4,-3,-3,-1,-1,-3,-1,-2,-3,-1,-1,-2,-2,-1],
    [-1, 1, 0, 0,-3, 5, 2,-2, 0,-3,-2, 1, 0,-3,-1, 0,-1,-2,-1,-2],
    [-1, 0, 0, 2,-4, 2, 5,-2, 0,-3,-3, 1,-2,-3,-1, 0,-1,-3,-2,-2],
    [ 0,-2, 0,-1,-3,-2,-2, 6,-2,-4,-4,-2,-3,-3,-2, 0,-2,-2,-3,-3],
    [-2, 0, 1,-1,-3, 0, 0,-2, 8,-3,-3,-1,-2,-1,-2,-1,-2,-2, 2,-3],
    [-1,-3,-3,-3,-1,-3,-3,-4,-3, 4, 2,-3, 1, 0,-3,-2,-1,-3,-1, 3],
    [-1,-2,-3,-4,-1,-2,-3,-4,-3, 2, 4,-2, 2, 0,-3,-2,-1,-2,-1, 1],
    [-1, 2, 0,-1,-3, 1, 1,-2,-1,-3,-2, 5,-1,-3,-1, 0,-1,-3,-2,-2],
    [-1,-1,-2,-3,-1, 0,-2,-3,-2, 1, 2,-1, 5, 0,-2,-1,-1,-1,-1, 1],
    [-2,-3,-3,-3,-2,-3,-3,-3,-1, 0, 0,-3, 0, 6,-4,-2,-2, 1, 3,-1],
    [-1,-2,-2,-1,-3,-1,-1,-2,-2,-3,-3,-1,-2,-4, 7,-1,-1,-4,-3,-2],
    [ 1,-1, 1, 0,-1, 0, 0, 0,-1,-2,-2, 0,-1,-2,-1, 4, 1,-3,-2,-2],
    [ 0,-1, 0,-1,-1,-1,-1,-2,-2,-1,-1,-1,-1,-2,-1, 1, 5,-2,-2, 0],
    [-3,-3,-4,-4,-2,-2,-3,-2,-2,-3,-2,-3,-1, 1,-4,-3,-2,11, 2,-3],
    [-2,-2,-2,-3,-2,-1,-2,-3, 2,-1,-1,-2,-1, 3,-3,-2,-2, 2, 7,-1],
    [ 0,-3,-3,-3,-1,-2,-2,-3,-3, 3, 1,-2, 1,-1,-2,-2, 0,-3,-1, 4],
];

fn score(a: u8, b: u8) -> i32 {
    let index = |c: u8| BLOSUM_ORDER.iter().position(|&x| x == c);
    match (index(a), index(b)) {
        (Some(i), Some(j)) => BLOSUM62[i][j] as i32,
        _ if a == b && a != b'X' => 5,
        _ => -1,
    }
}

/// Global alignment; returns the index pairs of aligned (non-gap) positions.
pub fn align_sequences(a: &[u8], b: &[u8]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0i32; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i as i32 * GAP;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j as i32 * GAP;
    }
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = (dp[i - 1][j - 1] + score(a[i - 1], b[j - 1])).max(dp[i - 1][j] + GAP).max(dp[i][j - 1] + GAP);
        }
    }
    let (mut i, mut j) = (n, m);
    let mut pairs = Vec::new();
    while i > 0 && j > 0 {
        if dp[i][j] == dp[i - 1][j - 1] + score(a[i - 1], b[j - 1]) {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i][j] == dp[i - 1][j] + GAP {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

/// Optimal rigid fit of `mobile` onto `target` (equal length, ≥ 3 points), and its RMSD.
pub fn fit(mobile: &[Vec3], target: &[Vec3]) -> (Transform, f32) {
    let n = mobile.len() as f64;
    let center = |pts: &[Vec3]| -> [f64; 3] {
        std::array::from_fn(|k| pts.iter().map(|p| p[k] as f64).sum::<f64>() / n)
    };
    let (cm, ct) = (center(mobile), center(target));
    // Cross-covariance S[a][b] = Σ p_a q_b over centered points, plus both spreads.
    let mut s = [[0.0f64; 3]; 3];
    let mut spread = 0.0;
    for (p, q) in mobile.iter().zip(target) {
        let p: [f64; 3] = std::array::from_fn(|k| p[k] as f64 - cm[k]);
        let q: [f64; 3] = std::array::from_fn(|k| q[k] as f64 - ct[k]);
        for a in 0..3 {
            for b in 0..3 {
                s[a][b] += p[a] * q[b];
            }
        }
        spread += p.iter().chain(&q).map(|x| x * x).sum::<f64>();
    }
    let [[xx, xy, xz], [yx, yy, yz], [zx, zy, zz]] = s;
    let mut nmat = [
        [xx + yy + zz, yz - zy, zx - xz, xy - yx],
        [yz - zy, xx - yy - zz, xy + yx, zx + xz],
        [zx - xz, xy + yx, -xx + yy - zz, yz + zy],
        [xy - yx, zx + xz, yz + zy, -xx - yy + zz],
    ];
    let (values, vectors) = jacobi4(&mut nmat);
    let best = (0..4).max_by(|&a, &b| values[a].total_cmp(&values[b])).unwrap();
    let [w, x, y, z] = std::array::from_fn(|k| vectors[k][best]);
    let r = [
        [w * w + x * x - y * y - z * z, 2.0 * (x * y - w * z), 2.0 * (x * z + w * y)],
        [2.0 * (x * y + w * z), w * w - x * x + y * y - z * z, 2.0 * (y * z - w * x)],
        [2.0 * (x * z - w * y), 2.0 * (y * z + w * x), w * w - x * x - y * y + z * z],
    ];
    let rotated: [f64; 3] = std::array::from_fn(|i| r[i][0] * cm[0] + r[i][1] * cm[1] + r[i][2] * cm[2]);
    let transform = Transform {
        rotation: r.map(|row| row.map(|v| v as f32)),
        translation: std::array::from_fn(|i| (ct[i] - rotated[i]) as f32),
    };
    let rmsd = ((spread - 2.0 * values[best]).max(0.0) / n).sqrt() as f32;
    (transform, rmsd)
}

/// Eigen-decomposition of a symmetric 4×4 matrix by cyclic Jacobi rotations.
/// Returns eigenvalues and the eigenvectors as columns.
fn jacobi4(a: &mut [[f64; 4]; 4]) -> ([f64; 4], [[f64; 4]; 4]) {
    let mut v = [[0.0; 4]; 4];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for _ in 0..50 {
        let off: f64 = (0..4).flat_map(|i| (0..4).filter(move |&j| j != i).map(move |j| (i, j))).map(|(i, j)| a[i][j] * a[i][j]).sum();
        if off < 1e-24 {
            break;
        }
        for p in 0..3 {
            for q in p + 1..4 {
                if a[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let (c, s) = (1.0 / (t * t + 1.0).sqrt(), t / (t * t + 1.0).sqrt());
                for row in a.iter_mut() {
                    let (akp, akq) = (row[p], row[q]);
                    row[p] = c * akp - s * akq;
                    row[q] = s * akp + c * akq;
                }
                let (rp, rq) = (a[p], a[q]);
                a[p] = std::array::from_fn(|k| c * rp[k] - s * rq[k]);
                a[q] = std::array::from_fn(|k| s * rp[k] + c * rq[k]);
                for row in v.iter_mut() {
                    let (vkp, vkq) = (row[p], row[q]);
                    row[p] = c * vkp - s * vkq;
                    row[q] = s * vkp + c * vkq;
                }
            }
        }
    }
    ([a[0][0], a[1][1], a[2][2], a[3][3]], v)
}

/// Sequence-aligns two residue lists (guide atom position, one-letter code) and fits them
/// with iterative outlier rejection. `None` when fewer than three residues pair up.
pub fn align(mobile: &[(Vec3, u8)], target: &[(Vec3, u8)]) -> Option<AlignResult> {
    let seq = |r: &[(Vec3, u8)]| r.iter().map(|x| x.1).collect::<Vec<_>>();
    let mut pairs = align_sequences(&seq(mobile), &seq(target));
    let paired = pairs.len();
    let mut cycles = 0;
    loop {
        if pairs.len() < 3 {
            return None;
        }
        let m: Vec<Vec3> = pairs.iter().map(|&(i, _)| mobile[i].0).collect();
        let t: Vec<Vec3> = pairs.iter().map(|&(_, j)| target[j].0).collect();
        let (transform, rmsd) = fit(&m, &t);
        let cutoff = (REJECT * rmsd as f64) as f32;
        let kept: Vec<(usize, usize)> = pairs
            .iter()
            .copied()
            .filter(|&(i, j)| {
                let p = transform.apply(mobile[i].0);
                let q = target[j].0;
                ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt() <= cutoff
            })
            .collect();
        if cycles == CYCLES || kept.len() == pairs.len() || kept.len() < 3 || rmsd < 1e-4 {
            return Some(AlignResult { transform, rmsd, aligned: pairs.len(), paired, cycles });
        }
        pairs = kept;
        cycles += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(n: usize) -> Vec<Vec3> {
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32 * 30.0
        };
        (0..n).map(|_| [next(), next(), next()]).collect()
    }

    fn rotation(angle: f32) -> Transform {
        let (s, c) = angle.sin_cos();
        // Rotation about the (1,1,1) axis via Rodrigues, plus a translation.
        let k = 1.0 / 3f32.sqrt();
        let (x, y, z) = (k, k, k);
        let t = 1.0 - c;
        Transform {
            rotation: [
                [c + x * x * t, x * y * t - z * s, x * z * t + y * s],
                [y * x * t + z * s, c + y * y * t, y * z * t - x * s],
                [z * x * t - y * s, z * y * t + x * s, c + z * z * t],
            ],
            translation: [5.0, -3.0, 12.0],
        }
    }

    #[test]
    fn fit_recovers_a_rigid_motion() {
        let target = points(50);
        let motion = rotation(1.1);
        let mobile: Vec<Vec3> = target.iter().map(|&p| motion.apply(p)).collect();
        let (t, rmsd) = fit(&mobile, &target);
        assert!(rmsd < 1e-3, "rmsd {rmsd}");
        for (m, q) in mobile.iter().zip(&target) {
            let p = t.apply(*m);
            assert!((0..3).all(|k| (p[k] - q[k]).abs() < 1e-3));
        }
    }

    #[test]
    fn sequence_alignment_handles_insertions() {
        let pairs = align_sequences(b"ACDEFGHIK", b"ACDEWFGHIK");
        assert_eq!(pairs.len(), 9);
        assert_eq!(pairs[4], (4, 5)); // F pairs with F, skipping the inserted W
    }

    #[test]
    fn outliers_are_rejected() {
        let target = points(40);
        let motion = rotation(0.4);
        let mut mobile: Vec<Vec3> = target.iter().map(|&p| motion.apply(p)).collect();
        // A flexible loop: five residues displaced far from where the rigid motion puts them.
        for p in &mut mobile[10..15] {
            p[0] += 15.0;
        }
        let seq = b"ACDEFGHIKLMNPQRSTVWYACDEFGHIKLMNPQRSTVWY";
        let with_seq = |pts: &[Vec3]| pts.iter().zip(seq).map(|(&p, &c)| (p, c)).collect::<Vec<_>>();
        let result = align(&with_seq(&mobile), &with_seq(&target)).unwrap();
        assert_eq!(result.paired, 40);
        assert_eq!(result.aligned, 35);
        assert!(result.rmsd < 1e-3, "rmsd {}", result.rmsd);
    }

    #[test]
    fn transforms_compose() {
        let (a, b) = (rotation(0.3), rotation(-0.8));
        let p = [1.0, 2.0, 3.0];
        let direct = b.apply(a.apply(p));
        let composed = b.after(&a).apply(p);
        assert!((0..3).all(|k| (direct[k] - composed[k]).abs() < 1e-4));
    }
}
