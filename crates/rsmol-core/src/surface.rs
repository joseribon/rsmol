//! Gaussian molecular surfaces.
//!
//! Each atom contributes a Gaussian density `exp(-B (d²/r² - 1))` to a grid; the surface is the
//! density = 1 isosurface. An isolated atom therefore gets exactly its van der Waals sphere, and
//! neighbors blend into a smooth skin (Krone et al., "QuickSurf"). The mesh is extracted with
//! naive surface nets: one vertex per cell the surface crosses, joined into quads across every
//! grid edge with a sign change. Each vertex records its dominant atom, for coloring and picking.
//!
//! The density pass is the expensive, embarrassingly parallel part, and the natural place for a
//! GPU/MLX backend; [`density_grid`] is kept separate for that reason.

use crate::Vec3;

#[derive(Clone, Copy, Debug)]
pub struct SurfaceParams {
    /// Finest grid spacing in Å; coarsened for large structures to respect `max_points`.
    pub resolution: f32,
    /// Gaussian sharpness B: higher hugs the atoms more tightly, lower is blobbier.
    pub blobbiness: f32,
    /// Upper bound on grid points, which bounds memory (12 bytes per point).
    pub max_points: usize,
}

impl Default for SurfaceParams {
    fn default() -> Self {
        SurfaceParams { resolution: 0.5, blobbiness: 1.6, max_points: 8_000_000 }
    }
}

/// An indexed triangle mesh; `atoms[v]` is the atom (index into the input coordinates) that
/// dominates vertex `v`.
#[derive(Clone, Debug, Default)]
pub struct SurfaceMesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub atoms: Vec<u32>,
    pub indices: Vec<u32>,
}

/// A scalar grid with per-point dominant atom.
pub struct DensityGrid {
    pub origin: Vec3,
    pub spacing: f32,
    pub dims: [usize; 3],
    pub density: Vec<f32>,
    /// Atom with the largest contribution at each point, `u32::MAX` where none reaches.
    pub dominant: Vec<u32>,
}

impl DensityGrid {
    fn index(&self, x: usize, y: usize, z: usize) -> usize {
        (z * self.dims[1] + y) * self.dims[0] + x
    }
}

/// Contributions below this fraction of the isolevel are ignored, which bounds each atom's reach.
const CUTOFF: f32 = 0.01;

/// Distance (in units of the atom radius) beyond which an atom's density is below `CUTOFF`.
fn reach(blobbiness: f32) -> f32 {
    (1.0 + (1.0 / CUTOFF).ln() / blobbiness).sqrt()
}

/// Splats the Gaussian density of `atoms` (indices into `coords`/`radii`) onto a grid.
pub fn density_grid(coords: &[Vec3], radii: &[f32], atoms: &[usize], params: SurfaceParams) -> Option<DensityGrid> {
    let first = *atoms.first()?;
    let reach = reach(params.blobbiness);
    let max_r = atoms.iter().map(|&i| radii[i]).fold(0.0, f32::max);
    let pad = max_r * reach + params.resolution;
    let (mut lo, mut hi) = (coords[first], coords[first]);
    for &i in atoms {
        for k in 0..3 {
            lo[k] = lo[k].min(coords[i][k]);
            hi[k] = hi[k].max(coords[i][k]);
        }
    }
    let lo = [lo[0] - pad, lo[1] - pad, lo[2] - pad];
    let extent = [hi[0] + pad - lo[0], hi[1] + pad - lo[1], hi[2] + pad - lo[2]];
    let volume = extent[0] * extent[1] * extent[2];
    let spacing = params.resolution.max((volume / params.max_points as f32).cbrt());
    let dims = extent.map(|e| (e / spacing).ceil() as usize + 1);
    let n = dims[0] * dims[1] * dims[2];

    let mut grid = DensityGrid {
        origin: lo,
        spacing,
        dims,
        density: vec![0.0; n],
        dominant: vec![u32::MAX; n],
    };
    // Largest single contribution so far, to pick each point's dominant atom.
    let mut best = vec![0.0f32; n];
    let b = params.blobbiness;
    for &i in atoms {
        let (c, r) = (coords[i], radii[i]);
        let inv_r2 = 1.0 / (r * r);
        let cutoff = r * reach;
        let range = |k: usize| {
            let from = ((c[k] - cutoff - lo[k]) / spacing).floor().max(0.0) as usize;
            let to = (((c[k] + cutoff - lo[k]) / spacing).ceil() as usize).min(dims[k] - 1);
            from..=to
        };
        let (xs, ys, zs) = (range(0), range(1), range(2));
        for z in zs {
            let dz = lo[2] + z as f32 * spacing - c[2];
            for y in ys.clone() {
                let dy = lo[1] + y as f32 * spacing - c[1];
                let dyz = dy * dy + dz * dz;
                let row = (z * dims[1] + y) * dims[0];
                for x in xs.clone() {
                    let dx = lo[0] + x as f32 * spacing - c[0];
                    let d2 = dx * dx + dyz;
                    if d2 > cutoff * cutoff {
                        continue;
                    }
                    let v = (-b * (d2 * inv_r2 - 1.0)).exp();
                    let p = row + x;
                    grid.density[p] += v;
                    if v > best[p] {
                        best[p] = v;
                        grid.dominant[p] = i as u32;
                    }
                }
            }
        }
    }
    Some(grid)
}

/// Extracts the density = 1 isosurface with naive surface nets.
pub fn extract(grid: &DensityGrid) -> SurfaceMesh {
    const ISO: f32 = 1.0;
    let [nx, ny, nz] = grid.dims;
    let at = |x: usize, y: usize, z: usize| grid.density[grid.index(x, y, z)];
    let mut mesh = SurfaceMesh::default();
    if nx < 2 || ny < 2 || nz < 2 {
        return mesh;
    }
    // Vertex index per cell (cell = cube with lowest corner at the grid point).
    let cell = |x: usize, y: usize, z: usize| (z * (ny - 1) + y) * (nx - 1) + x;
    let mut cell_vertex = vec![u32::MAX; (nx - 1) * (ny - 1) * (nz - 1)];
    const CORNERS: [[usize; 3]; 8] =
        [[0, 0, 0], [1, 0, 0], [0, 1, 0], [1, 1, 0], [0, 0, 1], [1, 0, 1], [0, 1, 1], [1, 1, 1]];
    const EDGES: [(usize, usize); 12] =
        [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)];

    let gradient = |x: usize, y: usize, z: usize| -> Vec3 {
        let d = |a: f32, b: f32| (a - b) * 0.5;
        let (xm, xp) = (x.saturating_sub(1), (x + 1).min(nx - 1));
        let (ym, yp) = (y.saturating_sub(1), (y + 1).min(ny - 1));
        let (zm, zp) = (z.saturating_sub(1), (z + 1).min(nz - 1));
        [d(at(xp, y, z), at(xm, y, z)), d(at(x, yp, z), at(x, ym, z)), d(at(x, y, zp), at(x, y, zm))]
    };

    for z in 0..nz - 1 {
        for y in 0..ny - 1 {
            for x in 0..nx - 1 {
                let values = CORNERS.map(|[dx, dy, dz]| at(x + dx, y + dy, z + dz));
                let inside = values.map(|v| v >= ISO);
                if inside.iter().all(|&b| b) || inside.iter().all(|&b| !b) {
                    continue;
                }
                // Vertex at the mean of the edge crossings, which keeps it inside the cell.
                let (mut sum, mut count) = ([0.0f32; 3], 0.0);
                for (a, b) in EDGES {
                    if inside[a] != inside[b] {
                        let t = (ISO - values[a]) / (values[b] - values[a]);
                        for k in 0..3 {
                            sum[k] += CORNERS[a][k] as f32 + t * (CORNERS[b][k] as f32 - CORNERS[a][k] as f32);
                        }
                        count += 1.0;
                    }
                }
                let local = sum.map(|s| s / count);
                let pos = [
                    grid.origin[0] + (x as f32 + local[0]) * grid.spacing,
                    grid.origin[1] + (y as f32 + local[1]) * grid.spacing,
                    grid.origin[2] + (z as f32 + local[2]) * grid.spacing,
                ];
                // Normal: density falls off outward, so it points along -∇ρ, averaged over the
                // cell corners; the dominant atom comes from the densest corner.
                let mut g = [0.0f32; 3];
                let mut densest = 0;
                for (k, [dx, dy, dz]) in CORNERS.iter().enumerate() {
                    let gc = gradient(x + dx, y + dy, z + dz);
                    for j in 0..3 {
                        g[j] += gc[j];
                    }
                    if values[k] > values[densest] {
                        densest = k;
                    }
                }
                let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-12);
                let [dx, dy, dz] = CORNERS[densest];
                cell_vertex[cell(x, y, z)] = mesh.positions.len() as u32;
                mesh.positions.push(pos);
                mesh.normals.push([-g[0] / len, -g[1] / len, -g[2] / len]);
                mesh.atoms.push(grid.dominant[grid.index(x + dx, y + dy, z + dz)]);
            }
        }
    }

    // A quad for each grid edge the surface crosses, joining the four cells around that edge.
    // Skipping edges on the grid boundary is safe: padding keeps the surface away from it.
    for z in 1..nz - 1 {
        for y in 1..ny - 1 {
            for x in 1..nx - 1 {
                let inside = at(x, y, z) >= ISO;
                let neighbors = [(x + 1, y, z), (x, y + 1, z), (x, y, z + 1)];
                for (axis, &(ax, ay, az)) in neighbors.iter().enumerate() {
                    if ax >= nx || ay >= ny || az >= nz || (at(ax, ay, az) >= ISO) == inside {
                        continue;
                    }
                    // The four cells sharing this edge differ in the two other axes.
                    let quad = match axis {
                        0 => [cell(x, y - 1, z - 1), cell(x, y, z - 1), cell(x, y, z), cell(x, y - 1, z)],
                        1 => [cell(x - 1, y, z - 1), cell(x - 1, y, z), cell(x, y, z), cell(x, y, z - 1)],
                        _ => [cell(x - 1, y - 1, z), cell(x, y - 1, z), cell(x, y, z), cell(x - 1, y, z)],
                    }
                    .map(|c| cell_vertex[c]);
                    if quad.contains(&u32::MAX) {
                        continue;
                    }
                    let [a, b, c, d] = if inside { quad } else { [quad[0], quad[3], quad[2], quad[1]] };
                    mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
                }
            }
        }
    }
    mesh
}

/// Density grid plus extraction for `atoms`.
pub fn gaussian_surface(coords: &[Vec3], radii: &[f32], atoms: &[usize], params: SurfaceParams) -> SurfaceMesh {
    density_grid(coords, radii, atoms, params).map(|g| extract(&g)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: Vec3, b: Vec3) -> f32 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    #[test]
    fn single_atom_gives_its_vdw_sphere() {
        let params = SurfaceParams { resolution: 0.25, ..Default::default() };
        let mesh = gaussian_surface(&[[1.0, 2.0, 3.0]], &[1.7], &[0], params);
        assert!(mesh.positions.len() > 500, "{} vertices", mesh.positions.len());
        for (p, n) in mesh.positions.iter().zip(&mesh.normals) {
            assert!((dist(*p, [1.0, 2.0, 3.0]) - 1.7).abs() < 0.05, "vertex at {}", dist(*p, [1.0, 2.0, 3.0]));
            // Normals point away from the center.
            let out = [p[0] - 1.0, p[1] - 2.0, p[2] - 3.0];
            assert!(out[0] * n[0] + out[1] * n[1] + out[2] * n[2] > 0.0);
        }
        assert!(mesh.atoms.iter().all(|&a| a == 0));
        assert_eq!(mesh.indices.len() % 3, 0);
    }

    #[test]
    fn mesh_is_closed() {
        // Every edge of a closed triangle mesh is shared by exactly two triangles.
        let coords = [[0.0, 0.0, 0.0], [1.5, 0.0, 0.0], [0.7, 1.3, 0.4]];
        let mesh = gaussian_surface(&coords, &[1.7, 1.5, 1.5], &[0, 1, 2], SurfaceParams::default());
        let mut edges = std::collections::HashMap::new();
        for t in mesh.indices.chunks(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        assert!(edges.values().all(|&c| c == 2));
        // Vertices are dominated by the nearest of the three atoms.
        assert_eq!(mesh.atoms.iter().collect::<std::collections::HashSet<_>>().len(), 3);
    }

    #[test]
    fn grid_is_coarsened_for_large_inputs() {
        let coords: Vec<Vec3> = (0..1000).map(|i| [(i % 10) as f32 * 30.0, (i / 10 % 10) as f32 * 30.0, (i / 100) as f32 * 30.0]).collect();
        let radii = vec![1.7; coords.len()];
        let atoms: Vec<usize> = (0..coords.len()).collect();
        let params = SurfaceParams { max_points: 1_000_000, ..Default::default() };
        let grid = density_grid(&coords, &radii, &atoms, params).unwrap();
        assert!(grid.density.len() <= 1_100_000, "{} points", grid.density.len());
        assert!(grid.spacing > params.resolution);
    }
}
