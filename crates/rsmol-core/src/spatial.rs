//! Uniform grid for neighbor queries.
//!
//! Atoms are bucketed into cubic cells (a counting sort, so memory is two flat arrays). A query
//! of radius `r <= cell` only has to look at the 27 cells around a point, which makes neighbor
//! search O(n) overall for molecular densities.

use crate::Vec3;

pub struct Grid {
    cell: f32,
    origin: [f32; 3],
    dims: [usize; 3],
    /// `atoms[starts[c]..starts[c + 1]]` are the atoms in cell `c`.
    starts: Vec<u32>,
    atoms: Vec<u32>,
}

impl Grid {
    /// Builds a grid over `coords` with cubic cells of edge `cell` Å.
    pub fn new(coords: &[Vec3], cell: f32) -> Grid {
        assert!(cell > 0.0, "grid cell size must be positive");
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for p in coords {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if coords.is_empty() {
            lo = [0.0; 3];
            hi = [0.0; 3];
        }
        let dims = std::array::from_fn(|k| ((hi[k] - lo[k]) / cell) as usize + 1);
        let mut grid = Grid { cell, origin: lo, dims, starts: Vec::new(), atoms: Vec::new() };

        let cell_of: Vec<usize> = coords.iter().map(|p| grid.index(grid.cell_coords(p))).collect();
        let mut counts = vec![0u32; dims[0] * dims[1] * dims[2] + 1];
        for &c in &cell_of {
            counts[c + 1] += 1;
        }
        for c in 1..counts.len() {
            counts[c] += counts[c - 1];
        }
        let mut fill = counts.clone();
        grid.atoms = vec![0; coords.len()];
        for (atom, &c) in cell_of.iter().enumerate() {
            grid.atoms[fill[c] as usize] = atom as u32;
            fill[c] += 1;
        }
        grid.starts = counts;
        grid
    }

    pub fn cell_size(&self) -> f32 {
        self.cell
    }

    /// Calls `f(atom, distance²)` for every atom within `radius` of `point`.
    /// `radius` may exceed the cell size; more cells are visited.
    pub fn within(&self, coords: &[Vec3], point: Vec3, radius: f32, mut f: impl FnMut(u32, f32)) {
        let r2 = radius * radius;
        let reach = (radius / self.cell).ceil() as isize;
        let center = self.cell_coords(&point);
        for c in self.cells_around(center, reach) {
            for &atom in self.cell_atoms(c) {
                let d2 = dist2(&coords[atom as usize], &point);
                if d2 <= r2 {
                    f(atom, d2);
                }
            }
        }
    }

    /// Calls `f(i, j, distance²)` once for every pair `i < j` closer than `radius`.
    pub fn pairs_within(&self, coords: &[Vec3], radius: f32, mut f: impl FnMut(u32, u32, f32)) {
        let r2 = radius * radius;
        let reach = (radius / self.cell).ceil() as isize;
        for (i, p) in coords.iter().enumerate() {
            let i = i as u32;
            for c in self.cells_around(self.cell_coords(p), reach) {
                for &j in self.cell_atoms(c) {
                    if j > i {
                        let d2 = dist2(&coords[j as usize], p);
                        if d2 <= r2 {
                            f(i, j, d2);
                        }
                    }
                }
            }
        }
    }

    fn cell_coords(&self, p: &Vec3) -> [isize; 3] {
        std::array::from_fn(|k| ((p[k] - self.origin[k]) / self.cell).floor() as isize)
    }

    /// Flat index of an in-bounds cell; out-of-range coordinates are clamped (points outside
    /// the original bounds can still be queried).
    fn index(&self, c: [isize; 3]) -> usize {
        let clamp = |k: usize| c[k].clamp(0, self.dims[k] as isize - 1) as usize;
        (clamp(2) * self.dims[1] + clamp(1)) * self.dims[0] + clamp(0)
    }

    fn cells_around(&self, center: [isize; 3], reach: isize) -> impl Iterator<Item = usize> + '_ {
        let range = |k: usize| {
            let lo = (center[k] - reach).max(0);
            let hi = (center[k] + reach).min(self.dims[k] as isize - 1);
            lo..=hi
        };
        let (xs, ys, zs) = (range(0), range(1), range(2));
        zs.flat_map(move |z| {
            let xs = xs.clone();
            ys.clone().flat_map(move |y| xs.clone().map(move |x| (x, y, z)))
        })
        .map(|(x, y, z)| self.index([x, y, z]))
    }

    fn cell_atoms(&self, c: usize) -> &[u32] {
        &self.atoms[self.starts[c] as usize..self.starts[c + 1] as usize]
    }
}

fn dist2(a: &Vec3, b: &Vec3) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random points (xorshift), so the test needs no dependencies.
    fn points(n: usize, extent: f32) -> Vec<Vec3> {
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32 * extent
        };
        (0..n).map(|_| [next(), next(), next()]).collect()
    }

    #[test]
    fn pairs_match_brute_force() {
        let pts = points(2000, 40.0);
        for (cell, radius) in [(2.0, 2.0), (2.0, 3.5), (5.0, 1.0)] {
            let grid = Grid::new(&pts, cell);
            let mut found = Vec::new();
            grid.pairs_within(&pts, radius, |i, j, _| found.push((i, j)));
            found.sort_unstable();
            let mut expected = Vec::new();
            for i in 0..pts.len() {
                for j in i + 1..pts.len() {
                    if dist2(&pts[i], &pts[j]) <= radius * radius {
                        expected.push((i as u32, j as u32));
                    }
                }
            }
            assert_eq!(found, expected, "cell {cell} radius {radius}");
        }
    }

    #[test]
    fn within_matches_brute_force_including_outside_points() {
        let pts = points(1000, 30.0);
        let grid = Grid::new(&pts, 3.0);
        for query in [[15.0, 15.0, 15.0], [-4.0, 2.0, 31.0], [0.0, 0.0, 0.0]] {
            let mut found = Vec::new();
            grid.within(&pts, query, 6.0, |a, _| found.push(a));
            found.sort_unstable();
            let expected: Vec<u32> =
                (0..pts.len() as u32).filter(|&a| dist2(&pts[a as usize], &query) <= 36.0).collect();
            assert_eq!(found, expected, "query {query:?}");
        }
    }

    #[test]
    fn empty_input() {
        let grid = Grid::new(&[], 2.0);
        grid.pairs_within(&[], 2.0, |_, _, _| panic!("no pairs expected"));
    }
}
