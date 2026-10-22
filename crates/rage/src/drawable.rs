//! WDR drawables (`gtaDrawable`) and WDD drawable dictionaries.
//! Layouts cross-checked with SparkIV's RageLib (read, not copied) and IV-Network's IV-RSCEN.txt.
//!
//! drawable  +0x08 shader group · +0x10 center · +0x20 bounds min · +0x30 bounds max
//!           +0x40 [4] ptr → PtrCollection<Model> per LOD (high → very low)
//! shaderGrp +0x04 embedded texture dictionary · +0x08 PtrCollection<ShaderFx>
//! shaderFx  +0x14 param ptrs · +0x1C param count · +0x24 param types (u8) · +0x34 param name
//!           hashes · +0x44 shader name
//! model     +0x04 PtrCollection<Geometry> · +0x10 u16 shader index per geometry
//! geometry  +0x0C vertex buffer · +0x1C index buffer · +0x36 primitive type
//! vbuffer   +0x04 u16 count · +0x08 data · +0x0C stride · +0x10 declaration
//! ibuffer   +0x04 u32 count · +0x08 data (u16 indices)
//! vdecl     u32 usage bits · u16 stride · u8 · u8 · u64 4-bit type per usage bit

use anyhow::{Result, bail, ensure};

use crate::rsc::Resource;
use crate::wtd::{self, Texture};

/// Name hashes of the texture shader parameters: diffuse (colour), normal map, specular map.
const PARAM_DIFFUSE: u32 = 0x2b51_70fd;
const PARAM_NORMAL: u32 = 0x46b7_c64f;
const PARAM_SPECULAR: u32 = 0x6087_99c6;
const PARAM_TEXTURE: u8 = 0;

#[derive(Debug, Clone, Default)]
pub struct Mesh {
    /// GTA space: Z up, metres.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u16>,
    pub shader: usize,
    /// Skinning (peds): 4 weights per vertex (sum 1) and the skeleton bone index for each.
    pub weights: Vec<[f32; 4]>,
    pub joints: Vec<[u16; 4]>,
    /// Per-vertex tangent (x, y, z, handedness), GTA space; present on normal-mapped geometry.
    pub tangents: Vec<[f32; 4]>,
    /// Per-vertex colour as the shaders see it (R, G, B, A). On map models this is baked
    /// lighting, not paint: IV's `gta_default` vertex shader scales the ambient light by
    /// red x a day factor + green x a night factor.
    pub colors: Vec<[u8; 4]>,
    /// Rigid models (grmModel +0x19 skin flag 0): the model's matrix index (+0x17), i.e. which
    /// entry of the owner's matrix set places it (a fragment's bone). None on skinned models.
    pub matrix: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct Shader {
    pub name: String,
    /// Diffuse texture name, lowercase, no extension.
    pub diffuse: Option<String>,
    /// Normal map and specular map texture names, on the shaders that use them (gta_normal*, gta_spec*).
    pub normal: Option<String>,
    pub specular: Option<String>,
    /// Every other texture parameter, in order: (parameter name hash, texture name). The terrain
    /// shaders (gta_terrain_va_2lyr / 3lyr / 4lyr) keep their layers here and have no diffuse.
    pub layers: Vec<(u32, String)>,
    /// The material's `emissiveMultiplier` setting (the self-lit shaders: colour = texture x
    /// vertex colour x this). 1.0 when the shader has none.
    pub emissive: f32,
}

#[derive(Debug, Clone)]
pub struct Drawable {
    /// Highest level of detail only.
    pub meshes: Vec<Mesh>,
    pub shaders: Vec<Shader>,
    /// Textures embedded in the drawable (vehicles/props); map models use external .wtd files.
    pub textures: Vec<Texture>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    /// The model's own lights (street lamps, signs, window lights).
    pub lights: Vec<Light>,
}

/// One light of a model (IV's light attribute, 0x6C bytes; the list hangs at drawable +0x80:
/// pointer, u16 count). Decoded 2026-10-01 by dumping lamp models: +0x04 position, +0x10
/// direction, +0x1C a second axis, +0x28 colour bytes R G B, +0x2C the distance it is drawn to
/// (100 / 300), +0x30 and +0x34 light-shaft strength and size, +0x38 range in metres, +0x3C
/// strength, +0x40 corona size, +0x48 cone angle in degrees, +0x4C flags, +0x5B type (1 point,
/// 2 spot). Range vs strength is INFERRED from sizes (a 15 m airport mast has 50 / 16: only 50
/// reaches the ground); the flags are not decoded.
#[derive(Clone, Debug)]
pub struct Light {
    /// Model space, GTA axes (Z up).
    pub pos: [f32; 3],
    pub dir: [f32; 3],
    pub color: [u8; 3],
    pub draw_dist: f32,
    pub range: f32,
    pub intensity: f32,
    /// Not understood yet, kept for study: the floats at +0x30, +0x34, +0x60, +0x64.
    pub extra: [f32; 4],
    pub corona: f32,
    pub cone: f32,
    pub flags: u32,
    pub kind: u8,
    /// The id of the bone the light hangs from (fragments: a lamp post's head); 0 on map models.
    pub bone: u16,
}

/// The lights of the drawable at `d` (empty if it has none, or if what's there doesn't look like
/// a light list: fragments keep other things at that offset).
pub fn read_lights(res: &Resource, d: u32) -> Vec<Light> {
    read_light_list(res, d, 0x80)
}

/// A fragment's lights (street lamp posts, traffic lights, wall lights are fragments). The list
/// hangs at the fragment root +0x210 (found 2026-10-01 with `liberty lightscan`), and each light
/// sits relative to a bone, named by the u16 id at light +0x68: it is moved by that bone's rest
/// transform (rotation, then position) into model space. Unlike the geometry (`rest_matrices`),
/// a light is in the bone's own turned frame: all three heads of bm_nytraflite3b's corner
/// bracket have light direction (0, -0.97, -0.26), and only the bone's 0 / 90 / 180 degree turn
/// puts each in front of its own lens (picture-checked, not read from the exe).
pub fn read_fragment_lights(res: &Resource, drawable: u32) -> Vec<Light> {
    let mut lights = read_light_list(res, Resource::ROOT, 0x210);
    if lights.iter().any(|l| l.bone != 0) {
        let bones = read_skeleton(res, drawable).unwrap_or_default();
        let frames = bone_frames(&bones);
        for l in &mut lights {
            if let Some(i) = bones.iter().position(|b| b.id == l.bone) {
                let (q, at) = frames[i];
                let p = qrot(q, l.pos);
                l.pos = [p[0] + at[0], p[1] + at[1], p[2] + at[2]];
                l.dir = qrot(q, l.dir);
            }
        }
    }
    lights
}

/// A rigid transform as the game stores it: three rows of (rotation row, translation).
pub type Mat34 = [[f32; 4]; 3];

/// A fragment's rest pose for drawing, one matrix per bone in bone order (model space). The game
/// keeps it in the file at fragType +0x1E4 (u32 count, matrices from +0x20, 0x30 bytes each,
/// rows of R | t) and hands it to every model it draws (exe 0x5fd190 -> 0x6a3fa0): a rigid model
/// is placed by entry `matrix` (grmModel +0x17). In the files seen these are moves only, even
/// for turned bones: the geometry is authored already facing its way (bm_nytraflite3b's head on
/// its 90-degree bone has its visors toward +x). Without that table: the skeleton's rest frames.
pub fn rest_matrices(res: &Resource, bones: &[Bone]) -> Vec<Mat34> {
    let stored = || -> Result<Vec<Mat34>> {
        let set = res.u32(Resource::ROOT, 0x1E4)?;
        ensure!(set != 0, "no matrix set");
        let count = res.u32(set, 0)? as usize;
        ensure!(count > 0 && count <= 256 && (bones.is_empty() || count == bones.len()), "matrix set count {count}");
        let f = |at: usize| -> Result<f32> { Ok(f32::from_bits(res.u32(set, at)?)) };
        (0..count)
            .map(|i| {
                let mut m = [[0.0; 4]; 3];
                for (r, row) in m.iter_mut().enumerate() {
                    for (c, v) in row.iter_mut().enumerate() {
                        *v = f(0x20 + i * 0x30 + r * 0x10 + c * 4)?;
                    }
                }
                ensure!(m.iter().flatten().all(|v| v.is_finite() && v.abs() < 5000.0), "not a matrix");
                Ok(m)
            })
            .collect()
    };
    stored().unwrap_or_else(|_| {
        bone_frames(bones)
            .into_iter()
            .map(|(q, at)| {
                let [x, y, z] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|a| qrot(q, a));
                [[x[0], y[0], z[0], at[0]], [x[1], y[1], z[1], at[1]], [x[2], y[2], z[2], at[2]]]
            })
            .collect()
    })
}

pub fn mat_point(m: &Mat34, p: [f32; 3]) -> [f32; 3] {
    let d = mat_dir(m, p);
    [d[0] + m[0][3], d[1] + m[1][3], d[2] + m[2][3]]
}

pub fn mat_dir(m: &Mat34, v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

/// Each bone's rest transform in model space: its rotation (the local quaternions composed down
/// from the root, x y z w) and its position (the skeleton's own absolute position).
pub fn bone_frames(bones: &[Bone]) -> Vec<([f32; 4], [f32; 3])> {
    let mut out: Vec<([f32; 4], [f32; 3])> = Vec::with_capacity(bones.len());
    for b in bones {
        let parent = b.parent.and_then(|p| out.get(p)).map_or([0.0, 0.0, 0.0, 1.0], |f| f.0);
        out.push((qmul(parent, b.local_rot), b.abs_pos));
    }
    out
}

/// Quaternion product a x b (x, y, z, w).
fn qmul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

/// A vector turned by a unit quaternion (x, y, z, w).
fn qrot(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let u = [q[0], q[1], q[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let t = cross(u, v).map(|x| 2.0 * x);
    let c = cross(u, t);
    [v[0] + q[3] * t[0] + c[0], v[1] + q[3] * t[1] + c[1], v[2] + q[3] * t[2] + c[2]]
}

fn read_light_list(res: &Resource, d: u32, at: usize) -> Vec<Light> {
    let read = || -> Result<Vec<Light>> {
        let list = res.u32(d, at)?;
        let count = res.u16(d, at + 4)? as usize;
        if list == 0 || count == 0 || count > 256 {
            return Ok(Vec::new());
        }
        let f = |p: u32, at: usize| -> Result<f32> { Ok(f32::from_bits(res.u32(p, at)?)) };
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let p = list + (i * 0x6C) as u32;
            let v = |at: usize| -> Result<[f32; 3]> { Ok([f(p, at)?, f(p, at + 4)?, f(p, at + 8)?]) };
            let l = Light {
                pos: v(0x04)?,
                dir: v(0x10)?,
                color: [res.u8(p, 0x28)?, res.u8(p, 0x29)?, res.u8(p, 0x2A)?],
                draw_dist: f(p, 0x2C)?,
                range: f(p, 0x38)?,
                intensity: f(p, 0x3C)?,
                extra: [f(p, 0x30)?, f(p, 0x34)?, f(p, 0x60)?, f(p, 0x64)?],
                corona: f(p, 0x40)?,
                cone: f(p, 0x48)?,
                flags: res.u32(p, 0x4C)?,
                kind: res.u8(p, 0x5B)?,
                bone: res.u16(p, 0x68)?,
            };
            let len = (l.dir[0] * l.dir[0] + l.dir[1] * l.dir[1] + l.dir[2] * l.dir[2]).sqrt();
            let sane = l.pos.iter().all(|x| x.is_finite() && x.abs() < 5000.0) && (len - 1.0).abs() < 0.05 && (0.0..2000.0).contains(&l.range) && (0.0..1000.0).contains(&l.intensity);
            ensure!(sane, "not a light");
            out.push(l);
        }
        Ok(out)
    };
    read().unwrap_or_default()
}

/// A WDR file: one drawable at the resource root.
pub fn read(res: &Resource) -> Result<Drawable> {
    read_at(res, Resource::ROOT)
}

/// A WFT file (`gtaFragType`: breakable props, vehicles). The main drawable (root +0xB4) owns
/// the shaders; props often keep their geometry only in the break-off children instead
/// (list at root +0xD4, count byte at +0x1F3, each child's drawable at child +0x90), which
/// share the main shader list. Every rigid model, main or child, is placed the way the game
/// draws it: by its own matrix index (grmModel +0x17) into the fragment's rest matrices
/// (`rest_matrices`). Not by the child's +0x0C: that byte is the child's group (the game only
/// uses it as the "broken off" bit), and it differs from the bone on e.g. bm_nytraflite6d,
/// whose signal head and walk box it swaps.
pub fn read_fragment(res: &Resource) -> Result<Drawable> {
    let root = Resource::ROOT;
    let d = res.u32(root, 0xB4)?;
    ensure!(d != 0, "fragment has no drawable");
    let mut main = read_at(res, d)?;
    main.lights = read_fragment_lights(res, d);
    if main.meshes.is_empty() {
        let list = res.u32(root, 0xD4)?;
        let count = res.u8(root, 0x1F3)? as usize;
        for i in 0..count {
            let child = res.u32(list, i * 4)?;
            let cd = res.u32(child, 0x90)?;
            if cd != 0 {
                main.meshes.extend(read_at(res, cd)?.meshes);
            }
        }
    }
    let bones = read_skeleton(res, d).unwrap_or_default();
    let mats = rest_matrices(res, &bones);
    for m in &mut main.meshes {
        let Some(mat) = m.matrix.and_then(|i| mats.get(i as usize)) else { continue };
        for p in &mut m.positions {
            *p = mat_point(mat, *p);
        }
        for n in &mut m.normals {
            *n = mat_dir(mat, *n);
        }
        for t in &mut m.tangents {
            let r = mat_dir(mat, [t[0], t[1], t[2]]);
            *t = [r[0], r[1], r[2], t[3]];
        }
    }
    if main.bounds_min == main.bounds_max {
        // fragments often leave the bounds empty; take them from the geometry
        let pts = main.meshes.iter().flat_map(|m| m.positions.iter());
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in pts {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if lo[0] <= hi[0] {
            (main.bounds_min, main.bounds_max) = (lo, hi);
        }
    }
    Ok(main)
}

/// One skeleton bone (layout from SparkIV's Bone.cs, 0xE0 bytes each).
#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    /// Bone tag that animation tracks refer to (+0x16).
    pub id: u16,
    pub parent: Option<usize>,
    /// Relative to the parent, GTA space.
    pub local_pos: [f32; 3],
    /// Quaternion x, y, z, w relative to the parent.
    pub local_rot: [f32; 4],
    /// In model space (rest pose).
    pub abs_pos: [f32; 3],
    /// Rest orientation as Euler angles (+0x70, radians) — the frame the limits are measured in.
    pub orient: [f32; 3],
    /// Rotation limits per axis (+0xB0 min, +0xC0 max, radians; RAGE .skel "rotmin"/"rotmax").
    /// Authored round numbers (e.g. hip -100..45 deg, knee -140..0 on one axis only).
    pub rot_min: [f32; 3],
    pub rot_max: [f32; 3],
    /// Raw words at +0x04, +0x08, +0x0C, +0x14 (flags / DOF bits, still being decoded).
    pub raw: [u32; 4],
}

/// The skeleton of a drawable (+0x0C): +0x00 bone array, +0x14 u16 bone count.
/// Bone: +0x00 name ptr · +0x10 parent ptr · +0x20 local pos · +0x40 local quat · +0x60 abs pos.
pub fn read_skeleton(res: &Resource, drawable: u32) -> Result<Vec<Bone>> {
    let skel = res.u32(drawable, 0x0C)?;
    if skel == 0 {
        return Ok(Vec::new());
    }
    let bones = res.u32(skel, 0x00)?;
    let count = res.u16(skel, 0x14)? as usize;
    let f = |p: u32, at: usize| -> Result<f32> { Ok(f32::from_bits(res.u32(p, at)?)) };
    (0..count)
        .map(|i| {
            let b = bones + (i * 0xE0) as u32;
            let parent = match res.u32(b, 0x10)? {
                0 => None,
                p => Some(((p - bones) / 0xE0) as usize),
            };
            Ok(Bone {
                name: res.cstr(res.u32(b, 0x00)?)?.to_ascii_lowercase(),
                id: res.u16(b, 0x16)?,
                parent,
                local_pos: [f(b, 0x20)?, f(b, 0x24)?, f(b, 0x28)?],
                local_rot: [f(b, 0x40)?, f(b, 0x44)?, f(b, 0x48)?, f(b, 0x4C)?],
                abs_pos: [f(b, 0x60)?, f(b, 0x64)?, f(b, 0x68)?],
                orient: [f(b, 0x70)?, f(b, 0x74)?, f(b, 0x78)?],
                rot_min: [f(b, 0xB0)?, f(b, 0xB4)?, f(b, 0xB8)?],
                rot_max: [f(b, 0xC0)?, f(b, 0xC4)?, f(b, 0xC8)?],
                raw: [res.u32(b, 0x04)?, res.u32(b, 0x08)?, res.u32(b, 0x0C)?, res.u32(b, 0x14)?],
            })
        })
        .collect()
}

/// A vehicle fragment: the body, and the one wheel mesh the game reuses for every wheel bone.
#[derive(Debug, Clone)]
pub struct Vehicle {
    pub body: Drawable,
    /// Wheel geometry in the wheel bone's local space (shares `body.shaders`).
    pub wheel: Option<Vec<Mesh>>,
    pub bones: Vec<Bone>,
}

impl Vehicle {
    /// Wheel bones (`wheel_lf`, `wheel_rr`, `wheel_lm`...) with their rest positions.
    pub fn wheels(&self) -> impl Iterator<Item = &Bone> {
        self.bones.iter().filter(|b| b.name.starts_with("wheel_") && !b.name.starts_with("wheelmesh"))
    }
}

/// Fragment children: list at root +0xD4, count byte at +0x1F3; child +0x0E = bone index,
/// child +0x90 = its drawable. Only the front-left wheel child carries geometry.
pub fn read_vehicle(res: &Resource) -> Result<Vehicle> {
    let root = Resource::ROOT;
    let d = res.u32(root, 0xB4)?;
    ensure!(d != 0, "fragment has no drawable");
    let body = read_at(res, d)?;
    let bones = read_skeleton(res, d)?;
    let list = res.u32(root, 0xD4)?;
    let count = res.u8(root, 0x1F3)? as usize;
    let mut wheel = None;
    for i in 0..count {
        let child = res.u32(list, i * 4)?;
        let bone = res.u16(child, 0x0E)? as usize;
        let cd = res.u32(child, 0x90)?;
        let is_wheel = bones.get(bone).is_some_and(|b| b.name.starts_with("wheel"));
        if is_wheel && cd != 0 {
            let meshes = read_at(res, cd)?.meshes;
            if !meshes.is_empty() {
                wheel = Some(meshes);
                break;
            }
        }
    }
    Ok(Vehicle { body, wheel, bones })
}

/// A WDD file: `pgDictionary<gtaDrawable>`. Returns (name hash, drawable) pairs.
pub fn read_dictionary(res: &Resource) -> Result<Vec<(u32, Drawable)>> {
    let root = Resource::ROOT;
    let hashes = res.u32(root, 0x10)?;
    let list = res.u32(root, 0x18)?;
    let count = res.u16(root, 0x1C)? as usize;
    (0..count)
        .map(|i| Ok((res.u32(hashes, i * 4)?, read_at(res, res.u32(list, i * 4)?)?)))
        .collect()
}

pub fn read_at(res: &Resource, d: u32) -> Result<Drawable> {
    let vec3 = |at: usize| -> Result<[f32; 3]> {
        Ok([0, 4, 8].map(|o| f32::from_bits(res.u32(d, at + o).unwrap_or(0))))
    };
    let (bounds_min, bounds_max) = (vec3(0x20)?, vec3(0x30)?);

    let (shaders, textures) = match res.u32(d, 0x08)? {
        0 => (Vec::new(), Vec::new()),
        group => read_shader_group(res, group)?,
    };

    // Best level of detail present; some drawables (light-only fittings) have none at all.
    let mut meshes = Vec::new();
    let Some(lod) = (0..4).map(|i| res.u32(d, 0x40 + i * 4)).find(|p| !matches!(p, Ok(0))).transpose()? else {
        return Ok(Drawable { meshes, shaders, textures, bounds_min, bounds_max, lights: read_lights(res, d) });
    };
    let models = res.u32(lod, 0)?;
    let model_count = res.u16(lod, 4)? as usize;
    for m in 0..model_count {
        let model = res.u32(models, m * 4)?;
        let geoms = res.u32(model, 0x04)?;
        let geom_count = res.u16(model, 0x08)? as usize;
        let mapping = res.u32(model, 0x10)?;
        let matrix = if res.u8(model, 0x19)? == 0 { Some(res.u8(model, 0x17)?) } else { None };
        for g in 0..geom_count {
            let shader = if mapping == 0 { 0 } else { res.u16(mapping, g * 2)? as usize };
            let mut mesh = read_geometry(res, res.u32(geoms, g * 4)?)?;
            mesh.shader = shader;
            mesh.matrix = matrix;
            meshes.push(mesh);
        }
    }
    Ok(Drawable { meshes, shaders, textures, bounds_min, bounds_max, lights: read_lights(res, d) })
}

fn read_shader_group(res: &Resource, group: u32) -> Result<(Vec<Shader>, Vec<Texture>)> {
    let textures = match res.u32(group, 0x04)? {
        0 => Vec::new(),
        txd => wtd::read_at(res, txd)?,
    };
    let list = res.u32(group, 0x08)?;
    let count = res.u16(group, 0x0C)? as usize;
    let mut shaders = Vec::with_capacity(count);
    for i in 0..count {
        let s = res.u32(list, i * 4)?;
        let name = match res.u32(s, 0x44)? {
            0 => String::new(),
            p => res.cstr(p)?,
        };
        let params = res.u32(s, 0x14)?;
        let n = res.u32(s, 0x1C)? as usize;
        let types = res.u32(s, 0x24)?;
        let names = res.u32(s, 0x34)?;
        let (mut diffuse, mut normal, mut specular) = (None, None, None);
        let mut layers = Vec::new();
        let mut emissive = 1.0;
        let emissive_name = crate::placement::hash("emissivemultiplier");
        for p in 0..n {
            if res.u8(types, p)? != PARAM_TEXTURE {
                // a number setting: a pointer to 4 floats
                if res.u32(names, p * 4)? == emissive_name {
                    if let Ok(v) = res.u32(params, p * 4).and_then(|at| res.u32(at, 0)).map(f32::from_bits) {
                        emissive = v;
                    }
                }
                continue;
            }
            let tex = res.u32(params, p * 4)?;
            if tex == 0 || res.u32(tex, 0x14)? == 0 {
                continue;
            }
            let texture = normalise_name(&res.cstr(res.u32(tex, 0x14)?)?);
            match res.u32(names, p * 4)? {
                PARAM_DIFFUSE => diffuse = Some(texture),
                PARAM_NORMAL => normal = Some(texture),
                PARAM_SPECULAR => specular = Some(texture),
                other => layers.push((other, texture)),
            }
        }
        // a terrain shader's first layer stands in for its colour until the layers are blended
        if diffuse.is_none() && name.contains("terrain") {
            diffuse = layers.first().map(|l| l.1.clone());
        }
        shaders.push(Shader { name, diffuse, normal, specular, layers, emissive });
    }
    Ok((shaders, textures))
}

pub fn normalise_name(n: &str) -> String {
    let n = n.trim_start_matches("pack:/").to_ascii_lowercase();
    n.strip_suffix(".dds").map(str::to_owned).unwrap_or(n)
}

/// Vertex element types (4 bits each in the declaration) and their byte sizes.
const TYPE_SIZE: [usize; 16] = [2, 4, 6, 8, 4, 8, 12, 16, 4, 4, 4, 0, 0, 0, 0, 0];
const T_HALF2: u64 = 1;
const T_HALF4: u64 = 3;
const T_FLOAT2: u64 = 5;
const T_FLOAT3: u64 = 6;
const T_FLOAT4: u64 = 7;

const T_COLOR: u64 = 9;

const BIT_POSITION: usize = 0;
const BIT_WEIGHTS: usize = 1;
const BIT_JOINTS: usize = 2;
const BIT_NORMAL: usize = 3;
const BIT_COLOR: usize = 4;
const BIT_UV0: usize = 6;
const BIT_TANGENT: usize = 14;

fn read_geometry(res: &Resource, g: u32) -> Result<Mesh> {
    let prim = res.u16(g, 0x36)?;
    ensure!(prim == 3, "unsupported primitive type {prim} (only triangle lists)");
    let vb = res.u32(g, 0x0C)?;
    let ib = res.u32(g, 0x1C)?;
    let count = res.u16(vb, 0x04)? as usize;
    let stride = res.u32(vb, 0x0C)? as usize;
    let decl = res.u32(vb, 0x10)?;
    let usage = res.u32(decl, 0)?;
    let types = res.u32(decl, 8)? as u64 | (res.u32(decl, 12)? as u64) << 32;

    // Byte offset and type of each element present, in usage-bit order.
    let mut layout = [None::<(usize, u64)>; 16];
    let mut off = 0;
    for (bit, slot) in layout.iter_mut().enumerate() {
        if usage & (1 << bit) != 0 {
            let t = (types >> (4 * bit)) & 0xF;
            *slot = Some((off, t));
            off += TYPE_SIZE[t as usize];
        }
    }
    ensure!(off == stride, "vertex layout adds up to {off} bytes, buffer stride is {stride}");

    let data = res.bytes(res.u32(vb, 0x08)?, count * stride)?;
    // Bone palette for skinned geometry: +0x38 → u16 skeleton bone per local index, count +0x3E.
    let palette: Vec<u16> = match res.u32(g, 0x38)? {
        0 => Vec::new(),
        p => {
            let n = res.u16(g, 0x3E)? as usize;
            (0..n).map(|i| res.u16(p, i * 2)).collect::<Result<_>>()?
        }
    };
    let read_vec = |v: &[u8], (o, t): (usize, u64), n: usize| -> Result<[f32; 4]> {
        let mut out = [0.0; 4];
        for (i, slot) in out.iter_mut().enumerate().take(n) {
            *slot = match t {
                T_FLOAT2 | T_FLOAT3 | T_FLOAT4 => {
                    f32::from_le_bytes(v[o + i * 4..o + i * 4 + 4].try_into().unwrap())
                }
                T_HALF2 | T_HALF4 => half(u16::from_le_bytes(v[o + i * 2..o + i * 2 + 2].try_into().unwrap())),
                _ => bail!("unsupported vertex element type {t}"),
            };
        }
        Ok(out)
    };

    let Some(pos) = layout[BIT_POSITION] else { bail!("geometry without positions") };
    let mut mesh = Mesh::default();
    for v in data.chunks_exact(stride) {
        let p = read_vec(v, pos, 3)?;
        mesh.positions.push([p[0], p[1], p[2]]);
        if let Some(n) = layout[BIT_NORMAL] {
            let n = read_vec(v, n, 3)?;
            mesh.normals.push([n[0], n[1], n[2]]);
        }
        if let Some(uv) = layout[BIT_UV0] {
            let uv = read_vec(v, uv, 2)?;
            mesh.uvs.push([uv[0], uv[1]]);
        }
        if let Some((o, t @ (T_FLOAT3 | T_FLOAT4))) = layout[BIT_TANGENT] {
            let n = if t == T_FLOAT4 { 4 } else { 3 };
            let tg = read_vec(v, (o, t), n)?;
            mesh.tangents.push([tg[0], tg[1], tg[2], if n == 4 { tg[3] } else { 1.0 }]);
        }
        if let Some((o, T_COLOR)) = layout[BIT_COLOR] {
            mesh.colors.push([v[o + 2], v[o + 1], v[o], v[o + 3]]);
        }
        // Skin: D3DCOLOR bytes stored B,G,R,A — the shaders read them .zyxw, so R,G,B,A order.
        if let (Some((wo, T_COLOR)), Some((jo, T_COLOR))) = (layout[BIT_WEIGHTS], layout[BIT_JOINTS]) {
            let swz = |o: usize| [v[o + 2], v[o + 1], v[o], v[o + 3]];
            let w = swz(wo).map(|b| b as f32 / 255.0);
            let sum: f32 = w.iter().sum();
            mesh.weights.push(if sum > 0.0 { w.map(|x| x / sum) } else { [1.0, 0.0, 0.0, 0.0] });
            let j = swz(jo);
            mesh.joints.push(j.map(|i| palette.get(i as usize).copied().unwrap_or(0)));
        }
    }

    let icount = res.u32(ib, 0x04)? as usize;
    let idata = res.bytes(res.u32(ib, 0x08)?, icount * 2)?;
    mesh.indices = idata.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    Ok(mesh)
}

/// IEEE half → f32.
fn half(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1F;
    let frac = (h & 0x3FF) as f32;
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        e => (1.0 + frac / 1024.0) * 2f32.powi(e as i32 - 15),
    }
}
