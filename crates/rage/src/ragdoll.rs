//! Ped ragdoll bodies from `.wft` fragments (`player.wft`, `componentpeds.img/*.wft`). No public
//! spec; reverse-engineered 2026-09-27 from `player.wft` with `liberty objdump`:
//!
//! fragType root  +0xB4 drawable (skeleton) · +0xCC → group names ("Char_Pelvis", …; group order,
//!                NOT child order) · +0xD0 → group records · +0xD4 → child records · +0xE4 archetype ·
//!                +0x1F3 u8 child count
//! archetype      (phArchetypeDamp) +0x0C bound · +0x1C mass (kg) · +0x20 1/mass ·
//!                +0x30 inertia xyz · +0x40 1/inertia xyz
//! bound          composite (type 0x0C, as in `.wbn`): +0x80 child bounds · +0x84 child matrices
//!                (4×4 rows, translation last) · +0x90 u16 count — one child bound per part
//! part bound     +0x04 u8 type: 1 capsule (along local Y), 3 box · +0x10 bbox max · +0x20 min
//!                capsule: radius = max.x, half cylinder length = max.y − radius
//! child record   +0x04 mass · +0x08 damaged mass · +0x0E u16 bone index · +0x90 drawable
//!
//! Niko: 89.2 kg over 21 parts (17 capsules; feet and hands are boxes).
//!
//! Joint limits: in the skeleton's bone records (drawable::Bone::rot_min / ot_max, bone +0xB0 /
//! +0xC0, radians per axis, RAGE .skel "rotmin"/"rotmax"), e.g. knee -140..0 on one axis (hinge,
//! phJoint1Dof), hip -100..45 / -80..15, shoulder -180..0 / -60..180, locked joints 0..0.

use anyhow::{Result, ensure};

use crate::Resource;

#[derive(Debug, Clone, Copy)]
pub enum Shape {
    /// Along the part's local Y axis.
    Capsule { radius: f32, half_length: f32 },
    Box { half: [f32; 3] },
    Other(u8),
}

#[derive(Debug, Clone)]
pub struct Part {
    /// The bone it rides on (skeleton name, e.g. "char_l_thigh").
    pub name: String,
    /// Index into the fragment drawable's skeleton.
    pub bone: u16,
    pub mass: f32,
    pub shape: Shape,
    /// The shape's placement in the ped's model space (GTA axes, Z up): rows, translation last.
    pub matrix: [[f32; 4]; 4],
}

#[derive(Debug, Clone)]
pub struct Ragdoll {
    pub mass: f32,
    pub inertia: [f32; 3],
    pub parts: Vec<Part>,
}

pub fn read_ragdoll(res: &Resource) -> Result<Ragdoll> {
    let root = Resource::ROOT;
    let f = |p: u32, o: usize| -> Result<f32> { Ok(f32::from_bits(res.u32(p, o)?)) };
    let arch = res.u32(root, 0xE4)?;
    ensure!(arch != 0, "fragment has no physics archetype");
    let mass = f(arch, 0x1C)?;
    let inertia = [f(arch, 0x30)?, f(arch, 0x34)?, f(arch, 0x38)?];
    let bound = res.u32(arch, 0x0C)?;
    ensure!(res.u8(bound, 0x04)? == 0x0C, "ragdoll bound is not a composite");
    let (kids, mats) = (res.u32(bound, 0x80)?, res.u32(bound, 0x84)?);
    let n = res.u16(bound, 0x90)? as usize;
    let children = res.u32(root, 0xD4)?;
    let skeleton = crate::drawable::read_skeleton(res, res.u32(root, 0xB4)?)?;
    let child_count = res.u8(root, 0x1F3)? as usize;
    ensure!(child_count == n, "{child_count} fragment children but {n} bounds");

    let mut parts = Vec::with_capacity(n);
    for i in 0..n {
        let b = res.u32(kids, i * 4)?;
        let (mx, my, mz) = (f(b, 0x10)?, f(b, 0x14)?, f(b, 0x18)?);
        let shape = match res.u8(b, 0x04)? {
            1 => Shape::Capsule { radius: mx, half_length: (my - mx).max(0.0) },
            3 => Shape::Box { half: [mx, my, mz] },
            t => Shape::Other(t),
        };
        let mut matrix = [[0.0; 4]; 4];
        for (r, row) in matrix.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = f(mats + (i * 64) as u32, r * 16 + c * 4)?;
            }
        }
        let child = res.u32(children, i * 4)?;
        let bone = res.u16(child, 0x0E)?;
        let name = skeleton.get(bone as usize).map(|b| b.name.clone()).unwrap_or_default();
        parts.push(Part { name, bone, mass: f(child, 0x04)?, shape, matrix });
    }
    Ok(Ragdoll { mass, inertia, parts })
}
