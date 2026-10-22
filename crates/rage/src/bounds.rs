//! Collision (`phBound`) from WBN/WBD resources. No public spec — reverse-engineered from the
//! game files (checked against the stored normals and polygon areas), 2026-09-26.
//!
//! bound header  +0x04 u8 type (0x0a geometry/BVH, 0x0c composite) · +0x10 bbox max · +0x20 min
//! geometry      +0x70 margin · +0x8C polygons · +0x90 vertex scale (per axis) · +0xA0 vertex
//!               offset · +0xB0 vertices (i16 × 3) · +0xC8 vertex count · +0xCC polygon count
//! polygon (32B) f32 normal[3] · f32 area · u16 vertex[4] · i16 neighbour[4]; triangles and quads
//!               share the layout, told apart by which one's area matches the stored area
//! composite     +0x80 child pointers · +0x84 child matrices (4×4, rows, translation last) ·
//!               +0x90 u16 child count
//! WBN root      +0x08 → one bound (usually a composite); world-space for map collision.

use anyhow::{Result, bail, ensure};

use crate::rsc::Resource;

const TYPE_GEOMETRY: u8 = 0x0a;
const TYPE_COMPOSITE: u8 = 0x0c;

/// Triangles in GTA world space (Z up).
#[derive(Debug, Default, Clone)]
pub struct CollisionMesh {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// Bound types we met but don't read yet (boxes, spheres, capsules...).
    pub skipped: usize,
    /// Polygons whose stored area matches neither the triangle nor the quad reading — should
    /// stay ~0; a jump means the vertex decoding is off.
    pub unmatched: usize,
    /// Polygons read.
    pub polygons: usize,
}

pub fn read_wbn(res: &Resource) -> Result<CollisionMesh> {
    let mut out = CollisionMesh::default();
    let root = res.u32(Resource::ROOT, 0x08)?;
    read_bound(res, root, &IDENTITY, &mut out)?;
    Ok(out)
}

type Mat = [[f32; 3]; 4]; // three rotation rows, then translation
const IDENTITY: Mat = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 0.0]];

fn apply(m: &Mat, p: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|k| p[0] * m[0][k] + p[1] * m[1][k] + p[2] * m[2][k] + m[3][k])
}

fn mul(a: &Mat, b: &Mat) -> Mat {
    // p·a then ·b
    let mut r = [[0.0; 3]; 4];
    for i in 0..3 {
        for k in 0..3 {
            r[i][k] = (0..3).map(|j| a[i][j] * b[j][k]).sum();
        }
    }
    r[3] = apply(b, a[3]);
    r
}

fn f32_at(res: &Resource, p: u32, at: usize) -> Result<f32> {
    Ok(f32::from_bits(res.u32(p, at)?))
}

fn vec3(res: &Resource, p: u32, at: usize) -> Result<[f32; 3]> {
    Ok([f32_at(res, p, at)?, f32_at(res, p, at + 4)?, f32_at(res, p, at + 8)?])
}

fn read_bound(res: &Resource, b: u32, xf: &Mat, out: &mut CollisionMesh) -> Result<()> {
    match res.u8(b, 0x04)? {
        TYPE_COMPOSITE => {
            let children = res.u32(b, 0x80)?;
            let matrices = res.u32(b, 0x84)?;
            let count = res.u16(b, 0x90)? as usize;
            for i in 0..count {
                let child = res.u32(children, i * 4)?;
                if child == 0 {
                    continue;
                }
                let m = if matrices == 0 {
                    IDENTITY
                } else {
                    let row = |r: usize| vec3(res, matrices, i * 64 + r * 16);
                    [row(0)?, row(1)?, row(2)?, row(3)?]
                };
                read_bound(res, child, &mul(&m, xf), out)?;
            }
        }
        TYPE_GEOMETRY => read_geometry(res, b, xf, out)?,
        _ => out.skipped += 1,
    }
    Ok(())
}

fn tri_area(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> f32 {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let x = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    0.5 * (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt()
}

fn read_geometry(res: &Resource, g: u32, xf: &Mat, out: &mut CollisionMesh) -> Result<()> {
    let polys = res.u32(g, 0x8C)?;
    let scale = vec3(res, g, 0x90)?;
    let offset = vec3(res, g, 0xA0)?;
    let verts = res.u32(g, 0xB0)?;
    let nv = res.u32(g, 0xC8)? as usize;
    let np = res.u32(g, 0xCC)? as usize;
    ensure!(nv < 65536 && np < 1 << 20, "implausible bound sizes ({nv} vertices, {np} polygons)");
    if nv == 0 || np == 0 {
        return Ok(());
    }

    let raw = res.bytes(verts, nv * 6)?;
    let local: Vec<[f32; 3]> = raw
        .chunks_exact(6)
        .map(|c| [0, 1, 2].map(|k| i16::from_le_bytes([c[k * 2], c[k * 2 + 1]]) as f32 * scale[k] + offset[k]))
        .collect();
    let base = out.vertices.len() as u32;
    out.vertices.extend(local.iter().map(|&p| apply(xf, p)));

    out.polygons += np;
    let pd = res.bytes(polys, np * 32)?;
    for p in pd.chunks_exact(32) {
        let area = f32::from_le_bytes(p[12..16].try_into().unwrap());
        let idx: [usize; 4] = [0, 1, 2, 3].map(|k| u16::from_le_bytes([p[16 + k * 2], p[17 + k * 2]]) as usize);
        if idx.iter().any(|&i| i >= nv) {
            bail!("polygon vertex index out of range ({idx:?} of {nv})");
        }
        let [a, b, c, d] = idx.map(|i| local[i]);
        let (t, t2) = (tri_area(a, b, c), tri_area(a, c, d));
        let close = |x: f32| (x - area).abs() <= 0.02 * area.max(0.01);
        let as_u32 = |i: usize| base + idx[i] as u32;
        out.triangles.push([as_u32(0), as_u32(1), as_u32(2)]);
        // a quad unless the first triangle alone accounts for the stored area
        if !close(t) && t2 > 1e-6 {
            out.triangles.push([as_u32(0), as_u32(2), as_u32(3)]);
        }
        if !close(t) && !close(t + t2) {
            out.unmatched += 1;
        }
    }
    Ok(())
}
