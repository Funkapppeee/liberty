//! Navigation meshes: `pc/data/cdimages/navmeshes.img`, 3,608 `.wnv` resources. The world is cut
//! into 100 m tiles named `sectors2x2_<X>_<Y>.wnv`, X and Y even sector numbers on a 50 m grid
//! from (-3000, -3000): tile (X, Y) covers x in [X*50-3000, +100), y likewise. This is where IV's
//! pedestrians walk: every polygon says whether it is pavement.
//!
//! Layout (worked out on `sectors2x2_56_50.wnv`, City Hall Park; no public spec used):
//! - root +0x40: f32 x3 size of the tile (100, 100, height range)
//! - root +0x58: -> vertices, u16 x3 each; position = min + v / 65535 * size
//! - root +0x60: -> corner indices, u16 each;   +0x68: u32 how many
//! - root +0x64: -> adjacency, 8 bytes per corner: u32 `area | poly << 16` of the polygon across
//!   the edge starting at that corner, then the same pair packed with type bits (not read)
//! - root +0x6c: -> polygons, 40 bytes each;    +0x7c: u32 how many
//! - root +0x70: -> sector tree; its first three f32 are the tile's minimum corner
//! - root +0x78: u32 vertex count;  +0x80: u32 area id = (Y/2) * 60 + X/2
//!
//! Polygon (40 bytes): u16 flags (bit 2 = pavement: drawn from above they are the sidewalks
//! round each block; bit 0 and the rest unread), u16 `corner count << 5` (4 bits) plus other bits,
//! u16 first corner index, then at +16 an i16 x6 bounding box in 1/8 m (min x, max x, min y, max
//! y, min z, max z). Checked: every polygon's corners fall inside its own box, and the corner
//! counts sum to the index count.

use anyhow::{Result, ensure};

use crate::rsc::Resource;

/// Bit 2 of a polygon's flags.
pub const PAVEMENT: u16 = 4;

#[derive(Debug, Clone)]
pub struct NavPoly {
    pub flags: u16,
    /// Corners, GTA space (Z up), in order.
    pub corners: Vec<[f32; 3]>,
    /// Per edge (corner i to corner i+1): the polygon across it, as (area id, polygon index).
    pub across: Vec<(u16, u16)>,
    /// The polygon's 40 bytes as stored (for research; most of it is unread).
    pub raw: [u8; 40],
    /// Per edge: the adjacency record's second half (the neighbour again, packed with type bits).
    pub link: Vec<u32>,
}

impl NavPoly {
    pub fn pavement(&self) -> bool {
        self.flags & PAVEMENT != 0
    }
    pub fn centre(&self) -> [f32; 3] {
        let n = self.corners.len().max(1) as f32;
        let mut c = [0.0; 3];
        for p in &self.corners {
            for k in 0..3 {
                c[k] += p[k] / n;
            }
        }
        c
    }
}

#[derive(Debug, Clone)]
pub struct NavMesh {
    pub area: u16,
    pub min: [f32; 3],
    pub size: [f32; 3],
    pub polys: Vec<NavPoly>,
}

/// The area id of the tile holding a GTA position, if it's on the map.
pub fn area_at(x: f32, y: f32) -> Option<u16> {
    let (c, r) = (((x + 3000.0) / 100.0).floor(), ((y + 3000.0) / 100.0).floor());
    ((0.0..60.0).contains(&c) && (0.0..60.0).contains(&r)).then(|| r as u16 * 60 + c as u16)
}

/// A tile's file name in navmeshes.img.
pub fn tile_name(area: u16) -> String {
    format!("sectors2x2_{}_{}.wnv", (area % 60) * 2, (area / 60) * 2)
}

pub fn read(res: &Resource) -> Result<NavMesh> {
    let root = Resource::ROOT;
    let f = |p: u32, at: usize| -> Result<f32> { Ok(f32::from_bits(res.u32(p, at)?)) };
    let size = [f(root, 0x40)?, f(root, 0x44)?, f(root, 0x48)?];
    let tree = res.u32(root, 0x70)?;
    let min = [f(tree, 0)?, f(tree, 4)?, f(tree, 8)?];
    let (verts, indices, adj, polys) = (res.u32(root, 0x58)?, res.u32(root, 0x60)?, res.u32(root, 0x64)?, res.u32(root, 0x6C)?);
    let (n_idx, n_vert, n_poly) = (res.u32(root, 0x68)? as usize, res.u32(root, 0x78)? as usize, res.u32(root, 0x7C)? as usize);
    ensure!(n_poly < 100_000 && n_idx < 1_000_000 && n_vert < 200_000, "navmesh counts out of range");
    let vb = res.bytes(verts, n_vert * 6)?;
    let ib = res.bytes(indices, n_idx * 2)?;
    let ab = res.bytes(adj, n_idx * 8)?;
    let pb = res.bytes(polys, n_poly * 40)?;
    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    let vertex = |i: usize| -> [f32; 3] {
        let v = [u16_at(vb, i * 6), u16_at(vb, i * 6 + 2), u16_at(vb, i * 6 + 4)];
        [0, 1, 2].map(|k| min[k] + v[k] as f32 / 65535.0 * size[k])
    };
    let mut out = Vec::with_capacity(n_poly);
    for p in pb.chunks_exact(40) {
        let count = ((u16_at(p, 2) >> 5) & 0xF) as usize;
        let start = u16_at(p, 4) as usize;
        ensure!(start + count <= n_idx, "navmesh polygon corners run past the index array");
        let mut corners = Vec::with_capacity(count);
        let mut across = Vec::with_capacity(count);
        let mut link = Vec::with_capacity(count);
        for k in start..start + count {
            let vi = u16_at(ib, k * 2) as usize;
            ensure!(vi < n_vert, "navmesh corner index {vi} past {n_vert} vertices");
            corners.push(vertex(vi));
            across.push((u16_at(ab, k * 8) & 0x0FFF, u16_at(ab, k * 8 + 2)));
            link.push(u32::from_le_bytes([ab[k * 8 + 4], ab[k * 8 + 5], ab[k * 8 + 6], ab[k * 8 + 7]]));
        }
        out.push(NavPoly { flags: u16_at(p, 0), corners, across, raw: p.try_into().unwrap(), link });
    }
    Ok(NavMesh { area: res.u32(root, 0x80)? as u16, min, size, polys: out })
}
