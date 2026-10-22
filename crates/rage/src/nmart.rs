//! NaturalMotion Euphoria character rigs (`.art`, also `.prt`), as shipped in Red Dead
//! Redemption's `naturalmotion.rpf` (`cowboy.art`, `cowgirl.art`, `horse.art`) and, as plain files,
//! in GTA IV's `pc/data/naturalmotion/` (`Fred.art` = male peds, `Wilma.art` = female; the runtime
//! tags the character kind 1 = Fred, 2 = Wilma, 3 = Cowboy from the asset name). No public spec;
//! reverse-engineered 2026-09-27 from `cowboy.art`:
//!
//! - u32 file size, then `07` "NMEUPHORIA"… A tagged little-endian byte stream: `03` + i32,
//!   `05` + f32, `07` + u32 length + bytes (strings incl. NUL, or raw blobs), `10` + vec3 (3 f32),
//!   `16` + 4×4 matrix; untagged flag bytes in between.
//! - The part list is spelled out (`Buttocks`, `Spine0`–`Spine3`, `Neck`, `Head`, clavicles, upper/lower
//!   arms, hands, thighs, shins, feet: the same 21 parts as GTA IV's ped ragdoll) and each part's
//!   collision is embedded as RAGE text bound (`type: capsule`, `length`, `radius`, `matrix`).
//! - Joint record: name string, `03` i32 · `03` i32 (part indices), type byte `01`, kind byte
//!   (`33` = 3-DOF swing/twist, `31` = hinge), vec3s (position, then twist and lean axes; hinges
//!   have two), 4 flag bytes, then a `07` blob: 12 unused bytes, then f32s.
//!   3-DOF (68 bytes): lean1 min/max, lean2 min/max, twist min/max (radians), three 1.0 scales,
//!   then the muscle block `300 300 d 20 100` (d = 0.5 hips/shoulders/neck, 1 spine/clavicles,
//!   5 ankles, 10 wrists — reads as muscle damping). Hinge (32 bytes): limit, then the muscle block.

use anyhow::{Result, bail, ensure};

#[derive(Debug, Clone)]
pub struct Joint {
    pub name: String,
    pub parts: [i32; 2],
    /// Hinge (elbows, knees) instead of a 3-DOF swing/twist joint.
    pub hinge: bool,
    /// Position and axes (joint frame), as stored.
    pub vectors: Vec<[f32; 3]>,
    /// 3-DOF: [lean1 min, lean1 max, lean2 min, lean2 max, twist min, twist max] (radians).
    /// Hinge: [limit] as stored.
    pub limits: Vec<f32>,
    /// The rest of the blob: 3-DOF `[1, 1, 1, 300, 300, d, 20, 100]`, hinge `[300, 0.5, 20, 100]`.
    pub muscle: Vec<f32>,
}

/// The joint names in a biped rig, as they appear in the file.
pub const BIPED_JOINTS: [&str; 20] = [
    "spine_0", "spine_1", "spine_2", "spine_3", "neck_lower", "neck_upper", "clavicle_jnt_left", "shoulder_left",
    "elbow_left", "wrist_left", "clavicle_jnt_right", "shoulder_right", "elbow_right", "wrist_right", "hip_left",
    "knee_left", "ankle_left", "hip_right", "knee_right", "ankle_right",
];

fn f32_at(d: &[u8], o: usize) -> Result<f32> {
    Ok(f32::from_le_bytes(d.get(o..o + 4).ok_or_else(|| anyhow::anyhow!("past end"))?.try_into()?))
}

fn i32_at(d: &[u8], o: usize) -> Result<i32> {
    Ok(i32::from_le_bytes(d.get(o..o + 4).ok_or_else(|| anyhow::anyhow!("past end"))?.try_into()?))
}

/// Read one named joint record (the joint names are unique strings in the file).
pub fn read_joint(d: &[u8], name: &str) -> Result<Joint> {
    let mut key = name.as_bytes().to_vec();
    key.push(0);
    let Some(at) = d.windows(key.len()).position(|w| w == key.as_slice()) else { bail!("joint {name} not found") };
    let mut o = at + key.len();
    ensure!(d.get(o) == Some(&0x03) && d.get(o + 5) == Some(&0x03), "{name}: expected two part indices");
    let parts = [i32_at(d, o + 1)?, i32_at(d, o + 6)?];
    let hinge = match d.get(o + 11) {
        Some(0x33) => false,
        Some(0x31) => true,
        k => bail!("{name}: unknown joint kind {k:?}"),
    };
    o += 12;
    let mut vectors = Vec::new();
    while d.get(o) == Some(&0x10) {
        vectors.push([f32_at(d, o + 1)?, f32_at(d, o + 5)?, f32_at(d, o + 9)?]);
        o += 13;
    }
    o += 4; // flag bytes
    ensure!(d.get(o) == Some(&0x07), "{name}: expected the joint blob");
    let len = u32::from_le_bytes(d[o + 1..o + 5].try_into()?) as usize;
    let blob = d.get(o + 5..o + 5 + len).ok_or_else(|| anyhow::anyhow!("{name}: blob past end"))?;
    let floats: Vec<f32> = (12..len).step_by(4).filter_map(|k| f32_at(blob, k).ok()).collect();
    let n_lim = if hinge { 1 } else { 6 };
    ensure!(floats.len() > n_lim, "{name}: short blob");
    Ok(Joint { name: name.to_string(), parts, hinge, vectors, limits: floats[..n_lim].to_vec(), muscle: floats[n_lim..].to_vec() })
}

/// A biped rig: its joints, and its 21 parts' rest frames (IV's part order; each [row a, row b,
/// row c, position], GTA axes, root-relative).
#[derive(Debug, Clone, Default)]
pub struct Biped {
    pub joints: Vec<Joint>,
    pub part_frames: Vec<[f32; 12]>,
    /// Each part's mass in kg (IV's part order), the physics masses IV's Euphoria body runs with
    /// (77.7 kg on Fred; matches the live game's articulated-body links).
    pub part_masses: Vec<f32>,
}

pub fn read_biped_rig(d: &[u8]) -> Result<Biped> {
    Ok(Biped { joints: read_biped(d)?, part_frames: read_part_frames(d), part_masses: read_part_masses(d) })
}

/// Each part's mass: the `05` float right after its first `16` matrix (the collision offset).
pub fn read_part_masses(d: &[u8]) -> Vec<f32> {
    part_matrices(d)
        .iter()
        .step_by(2)
        .filter_map(|&at| (d.get(at + 65) == Some(&0x05)).then(|| f32::from_le_bytes(d[at + 66..at + 70].try_into().unwrap())))
        .take(21)
        .collect()
}

/// Offsets of the part matrices (`16` tag + 16 floats, three unit rows).
fn part_matrices(d: &[u8]) -> Vec<usize> {
    let mut at = Vec::new();
    let mut i = 0;
    while i + 65 <= d.len() {
        if d[i] == 0x16 {
            let m: Vec<f32> = (0..16).map(|k| f32::from_le_bytes(d[i + 1 + 4 * k..i + 5 + 4 * k].try_into().unwrap())).collect();
            let unit = |r: usize| ((m[r] * m[r] + m[r + 1] * m[r + 1] + m[r + 2] * m[r + 2]).sqrt() - 1.0).abs() < 0.02;
            if unit(0) && unit(4) && unit(8) && m.iter().all(|v| v.abs() < 5.0) {
                at.push(i);
                i += 65;
                continue;
            }
        }
        i += 1;
    }
    at
}

/// The parts' rest frames: each part has two `16` 4x4 matrices (a collision offset, then the part's
/// rest frame, rows + translation); a matrix is taken when its three rows are unit length.
pub fn read_part_frames(d: &[u8]) -> Vec<[f32; 12]> {
    let mut mats = Vec::new();
    let mut i = 0;
    while i + 65 <= d.len() {
        if d[i] == 0x16 {
            let m: Vec<f32> = (0..16).map(|k| f32::from_le_bytes(d[i + 1 + 4 * k..i + 5 + 4 * k].try_into().unwrap())).collect();
            let unit = |r: usize| ((m[r] * m[r] + m[r + 1] * m[r + 1] + m[r + 2] * m[r + 2]).sqrt() - 1.0).abs() < 0.02;
            if unit(0) && unit(4) && unit(8) && m.iter().all(|v| v.abs() < 5.0) {
                mats.push([m[0], m[1], m[2], m[4], m[5], m[6], m[8], m[9], m[10], m[12], m[13], m[14]]);
                i += 65;
                continue;
            }
        }
        i += 1;
    }
    (0..21).filter_map(|k| mats.get(2 * k + 1).copied()).collect()
}

/// All biped joints of a character rig (`cowboy.art`).
pub fn read_biped(d: &[u8]) -> Result<Vec<Joint>> {
    ensure!(d.windows(10).any(|w| w == b"NMEUPHORIA"), "not a NaturalMotion Euphoria file");
    BIPED_JOINTS.iter().map(|n| read_joint(d, n)).collect()
}
