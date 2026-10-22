//! Triangle-soup collision queries for a walking player.
//!
//! Each streamed map chunk keeps its triangles bucketed on a coarse XZ grid so that
//! ground and wall queries only need to look at nearby geometry instead of the whole
//! chunk. Coordinates are Bevy space (Y up), in metres.

use bevy::math::Vec3;
use std::collections::{HashMap, HashSet};

/// Size of one grid cell, in metres, on the XZ plane.
const CELL_SIZE: f32 = 8.0;
/// Maximum grid extent (in cells) a single triangle may be inserted into.
const MAX_CELL_SPAN: i32 = 64;
/// Triangles with an area below this are considered degenerate and dropped.
const MIN_TRIANGLE_AREA: f32 = 1e-6;

/// Triangles of one streamed map chunk, bucketed on an XZ grid for fast local queries.
pub struct ChunkCollision {
    tris: Vec<[Vec3; 3]>,
    grid: HashMap<(i32, i32), Vec<u32>>,
}

fn cell_of(x: f32, z: f32) -> (i32, i32) {
    ((x / CELL_SIZE).floor() as i32, (z / CELL_SIZE).floor() as i32)
}

impl ChunkCollision {
    /// `vertices`/`triangles` are already in Bevy space. Cell size 8 m. A triangle goes into
    /// every cell its XZ bounding box overlaps (cap: skip-insert beyond 64x64 cells, log
    /// nothing). Drop degenerate triangles (area < 1e-6).
    pub fn new(vertices: &[[f32; 3]], triangles: &[[u32; 3]]) -> ChunkCollision {
        let mut tris = Vec::with_capacity(triangles.len());
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();

        for tri in triangles {
            let [ia, ib, ic] = *tri;
            let (Some(a), Some(b), Some(c)) = (
                vertices.get(ia as usize),
                vertices.get(ib as usize),
                vertices.get(ic as usize),
            ) else {
                continue;
            };
            let a = Vec3::from_array(*a);
            let b = Vec3::from_array(*b);
            let c = Vec3::from_array(*c);

            let area2 = (b - a).cross(c - a).length();
            if area2 * 0.5 < MIN_TRIANGLE_AREA {
                continue;
            }

            let index = tris.len() as u32;
            tris.push([a, b, c]);

            let min_x = a.x.min(b.x).min(c.x);
            let max_x = a.x.max(b.x).max(c.x);
            let min_z = a.z.min(b.z).min(c.z);
            let max_z = a.z.max(b.z).max(c.z);

            let (cx0, cz0) = cell_of(min_x, min_z);
            let (cx1, cz1) = cell_of(max_x, max_z);

            if (cx1 - cx0 + 1) > MAX_CELL_SPAN || (cz1 - cz0 + 1) > MAX_CELL_SPAN {
                continue;
            }

            for cx in cx0..=cx1 {
                for cz in cz0..=cz1 {
                    grid.entry((cx, cz)).or_default().push(index);
                }
            }
        }

        ChunkCollision { tris, grid }
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Triangle indices stored in a single grid cell, if any.
    fn cell(&self, cx: i32, cz: i32) -> &[u32] {
        self.grid.get(&(cx, cz)).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Barycentric coordinates of `p` (XZ-projected) with respect to triangle `(a, b, c)`
/// (also XZ-projected). Returns `None` if the projected triangle is degenerate.
fn barycentric_xz(a: Vec3, b: Vec3, c: Vec3, px: f32, pz: f32) -> Option<(f32, f32, f32)> {
    let (ax, az) = (a.x, a.z);
    let (bx, bz) = (b.x, b.z);
    let (cx, cz) = (c.x, c.z);

    let v0x = bx - ax;
    let v0z = bz - az;
    let v1x = cx - ax;
    let v1z = cz - az;
    let v2x = px - ax;
    let v2z = pz - az;

    let d00 = v0x * v0x + v0z * v0z;
    let d01 = v0x * v1x + v0z * v1z;
    let d11 = v1x * v1x + v1z * v1z;
    let d20 = v2x * v0x + v2z * v0z;
    let d21 = v2x * v1x + v2z * v1z;

    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-12 {
        return None;
    }

    let v = (d11 * d20 - d01 * d21) / denom;
    let w = (d00 * d21 - d01 * d20) / denom;
    let u = 1.0 - v - w;
    Some((u, v, w))
}

/// Highest walkable surface at (x, z) whose height is <= `from_y`, across all chunks.
/// A surface counts when the vertical line through (x, z) hits the triangle (barycentric test
/// in XZ) and the triangle normal's |y| >= 0.35 (so walls don't count). Returns None if nothing
/// below.
pub fn ground_height<'a>(
    chunks: impl IntoIterator<Item = &'a ChunkCollision> + Clone,
    x: f32,
    z: f32,
    from_y: f32,
) -> Option<f32> {
    let (cx, cz) = cell_of(x, z);
    let mut best: Option<f32> = None;

    for chunk in chunks {
        for &idx in chunk.cell(cx, cz) {
            let [a, b, c] = chunk.tris[idx as usize];
            let Some((u, v, w)) = barycentric_xz(a, b, c, x, z) else {
                continue;
            };
            const EPS: f32 = -1e-4;
            if u < EPS || v < EPS || w < EPS {
                continue;
            }

            let normal = (b - a).cross(c - a).normalize_or_zero();
            if normal.y.abs() < 0.35 {
                continue;
            }

            let y = u * a.y + v * b.y + w * c.y;
            if y <= from_y + 1e-4 {
                best = Some(best.map_or(y, |cur| cur.max(y)));
            }
        }
    }

    best
}

/// Closest point on triangle `(a, b, c)` to point `p`.
/// Ericson, "Real-Time Collision Detection" §5.1.5.
fn closest_point_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;

    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a; // vertex region a
    }

    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b; // vertex region b
    }

    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return a + ab * v; // edge region ab
    }

    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c; // vertex region c
    }

    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return a + ac * w; // edge region ac
    }

    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return b + (c - b) * w; // edge region bc
    }

    // Inside face region.
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    a + ab * v + ac * w
}

/// Push a vertical capsule-ish player out of walls. The player is approximated by spheres of
/// `radius` centred at `feet + Vec3::Y * h` for h in `heights` (e.g. [0.55, 1.2, 1.6]).
/// For each sphere, for each nearby triangle whose normal |y| < 0.7 (walls/steep), compute the
/// closest point on the triangle; if closer than radius, push the player horizontally (XZ only)
/// out by the overlap. Do 3 relaxation passes. Returns the corrected feet position.
pub fn push_out_of_walls<'a>(
    chunks: impl IntoIterator<Item = &'a ChunkCollision> + Clone,
    feet: Vec3,
    radius: f32,
    heights: &[f32],
) -> Vec3 {
    let mut feet = feet;

    for _pass in 0..3 {
        for &h in heights {
            let sphere_center = feet + Vec3::Y * h;
            let (cx, cz) = cell_of(sphere_center.x, sphere_center.z);
            let span = ((radius / CELL_SIZE).ceil() as i32).max(1);

            for chunk in chunks.clone() {
                // A triangle can be inserted into several overlapping cells; dedupe here so a
                // wide triangle isn't pushed against twice in one sphere/pass.
                let mut seen: HashSet<u32> = HashSet::new();
                for dx in -span..=span {
                    for dz in -span..=span {
                        for &idx in chunk.cell(cx + dx, cz + dz) {
                            if !seen.insert(idx) {
                                continue;
                            }
                            let [a, b, c] = chunk.tris[idx as usize];
                            let normal = (b - a).cross(c - a).normalize_or_zero();
                            if normal.y.abs() >= 0.7 {
                                continue;
                            }

                            let closest = closest_point_on_triangle(sphere_center, a, b, c);
                            let delta = sphere_center - closest;
                            let dist = delta.length();
                            if dist >= radius || dist < 1e-8 {
                                // If exactly on the surface, try to push along the triangle's
                                // horizontal normal component instead of doing nothing.
                                if dist < 1e-8 {
                                    let horiz = Vec3::new(normal.x, 0.0, normal.z);
                                    if horiz.length() > 1e-6 {
                                        let push = horiz.normalize() * radius;
                                        feet.x += push.x;
                                        feet.z += push.z;
                                    }
                                }
                                continue;
                            }

                            let overlap = radius - dist;
                            let horiz = Vec3::new(delta.x, 0.0, delta.z);
                            let horiz_len = horiz.length();
                            if horiz_len > 1e-6 {
                                let push = horiz / horiz_len * overlap;
                                feet.x += push.x;
                                feet.z += push.z;
                            } else {
                                // Directly above/below the closest point in XZ: fall back to the
                                // triangle normal's XZ direction, or skip if that's ~zero too.
                                let horiz_n = Vec3::new(normal.x, 0.0, normal.z);
                                if horiz_n.length() > 1e-6 {
                                    let push = horiz_n.normalize() * overlap;
                                    feet.x += push.x;
                                    feet.z += push.z;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    feet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_floor(y: f32, half_extent: f32) -> ChunkCollision {
        let verts = [
            [-half_extent, y, -half_extent],
            [half_extent, y, -half_extent],
            [half_extent, y, half_extent],
            [-half_extent, y, half_extent],
        ];
        let tris = [[0u32, 1, 2], [0, 2, 3]];
        ChunkCollision::new(&verts, &tris)
    }

    #[test]
    fn flat_floor_ground_height() {
        let floor = flat_floor(5.0, 10.0);
        assert_eq!(floor.triangle_count(), 2);

        let chunks = [floor];

        assert_eq!(ground_height(&chunks, 0.0, 0.0, 10.0), Some(5.0));
        assert_eq!(ground_height(&chunks, 0.0, 0.0, 4.0), None);
        assert_eq!(ground_height(&chunks, 100.0, 100.0, 10.0), None);
    }

    #[test]
    fn ramp_ground_height() {
        // Floor from x in [-10, 0] at y = 0, ramp from x in [0, 10] rising to y = 7.
        let verts = [
            [-10.0, 0.0, -10.0], // 0
            [0.0, 0.0, -10.0],   // 1
            [0.0, 0.0, 10.0],    // 2
            [-10.0, 0.0, 10.0],  // 3
            [0.0, 0.0, -10.0],   // 4 (ramp start, low)
            [10.0, 7.0, -10.0],  // 5 (ramp end, high)
            [10.0, 7.0, 10.0],   // 6
            [0.0, 0.0, 10.0],    // 7
        ];
        let tris = [
            [0u32, 1, 2],
            [0, 2, 3],
            [4, 5, 6],
            [4, 6, 7],
        ];
        let chunk = ChunkCollision::new(&verts, &tris);
        let chunks = [chunk];

        // Halfway along the ramp (x = 5) the height should be interpolated to ~3.5.
        let h = ground_height(&chunks, 5.0, 0.0, 100.0).expect("ramp hit");
        assert!((h - 3.5).abs() < 1e-4, "got {h}");

        // On the flat floor.
        let h = ground_height(&chunks, -5.0, 0.0, 100.0).expect("floor hit");
        assert!((h - 0.0).abs() < 1e-4, "got {h}");
    }

    fn vertical_wall_at_x1() -> ChunkCollision {
        // Quad in the plane x = 1, spanning z in [-5, 5], y in [0, 3].
        let verts = [
            [1.0, 0.0, -5.0],
            [1.0, 0.0, 5.0],
            [1.0, 3.0, 5.0],
            [1.0, 3.0, -5.0],
        ];
        let tris = [[0u32, 1, 2], [0, 2, 3]];
        ChunkCollision::new(&verts, &tris)
    }

    #[test]
    fn wall_pushes_player_out() {
        let wall = vertical_wall_at_x1();
        let chunks = [wall];

        let feet = Vec3::new(0.8, 0.0, 0.0);
        let corrected = push_out_of_walls(&chunks, feet, 0.35, &[0.55, 1.2]);
        assert!((corrected.x - 0.65).abs() < 0.01, "got {corrected:?}");
        assert!((corrected.z - 0.0).abs() < 1e-4, "got {corrected:?}");

        let far_feet = Vec3::new(-2.0, 0.0, 0.0);
        let unmoved = push_out_of_walls(&chunks, far_feet, 0.35, &[0.55, 1.2]);
        assert!((unmoved - far_feet).length() < 1e-4, "got {unmoved:?}");
    }

    #[test]
    fn wide_floor_far_corner() {
        let floor = flat_floor(2.0, 50.0);
        let chunks = [floor];

        let h = ground_height(&chunks, 49.0, 49.0, 10.0);
        assert_eq!(h, Some(2.0));
    }
}

/// The first triangle a ray hits within `max` metres: (distance, surface normal facing the ray).
/// Walks the grid cells under the ray (and their neighbours, so a triangle straddling a cell edge
/// isn't missed).
pub fn raycast<'a>(chunks: impl IntoIterator<Item = &'a ChunkCollision>, origin: Vec3, dir: Vec3, max: f32) -> Option<(f32, Vec3)> {
    let mut cells: HashSet<(i32, i32)> = HashSet::new();
    let steps = (max / (CELL_SIZE * 0.5)).ceil() as i32;
    for s in 0..=steps {
        let p = origin + dir * (s as f32 * CELL_SIZE * 0.5).min(max);
        let (cx, cz) = cell_of(p.x, p.z);
        for dx in -1..=1 {
            for dz in -1..=1 {
                cells.insert((cx + dx, cz + dz));
            }
        }
    }
    let mut best: Option<(f32, Vec3)> = None;
    for chunk in chunks {
        let mut seen: HashSet<u32> = HashSet::new();
        for &(cx, cz) in &cells {
            for &i in chunk.cell(cx, cz) {
                if !seen.insert(i) {
                    continue;
                }
                // Moller-Trumbore, either face
                let [a, b, c] = chunk.tris[i as usize];
                let (e1, e2) = (b - a, c - a);
                let h = dir.cross(e2);
                let det = e1.dot(h);
                if det.abs() < 1e-8 {
                    continue;
                }
                let s = origin - a;
                let u = s.dot(h) / det;
                if !(0.0..=1.0).contains(&u) {
                    continue;
                }
                let q = s.cross(e1);
                let v = dir.dot(q) / det;
                if v < 0.0 || u + v > 1.0 {
                    continue;
                }
                let t = e2.dot(q) / det;
                if t > 1e-4 && t <= max && best.is_none_or(|(bt, _)| t < bt) {
                    let n = e1.cross(e2).normalize_or_zero();
                    best = Some((t, if n.dot(dir) > 0.0 { -n } else { n }));
                }
            }
        }
    }
    best
}

/// Every triangle within `radius` (XZ) of `center`, across chunks (each once), in a fixed order: the
/// chunks come out of a HashMap in a different order every run, and the ragdoll's contacts follow
/// this order, so without it the same test fell differently each time (re/lab.sh).
pub fn triangles_near<'a>(chunks: impl IntoIterator<Item = &'a ChunkCollision>, center: Vec3, radius: f32) -> Vec<[Vec3; 3]> {
    let (cx0, cz0) = cell_of(center.x - radius, center.z - radius);
    let (cx1, cz1) = cell_of(center.x + radius, center.z + radius);
    let mut out = Vec::new();
    for chunk in chunks {
        let mut seen: HashSet<u32> = HashSet::new();
        for cx in cx0..=cx1 {
            for cz in cz0..=cz1 {
                for &i in chunk.cell(cx, cz) {
                    if seen.insert(i) {
                        out.push(chunk.tris[i as usize]);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.iter().flat_map(|v| v.to_array()).zip(b.iter().flat_map(|v| v.to_array())).map(|(x, y)| x.total_cmp(&y)).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal));
    out
}