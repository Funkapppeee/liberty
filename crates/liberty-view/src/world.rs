//! CPU side of the map: which chunks exist, what each places where, and loading their assets
//! off the main thread into ready-to-upload Bevy meshes and images.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use bevy::prelude::*;
use rage::drawable::Drawable;
use rage::index::GameIndex;
use rage::placement::{self, Instance};
use rage::wtd::{Format, Texture};

use crate::assets::{self, ImageKey, MatKey};

/// One placed object, visible while `near <= camera distance < far` (before the view-distance
/// multiplier). HD objects: near 0, far = draw distance — or, if they have a stand-in, the
/// stand-in's switch distance, so a whole group swaps at once and no holes open up.
/// Stand-ins: near = switch distance (furthest draw distance among what they replace).
#[derive(Clone)]
pub struct Placed {
    pub model: u32,
    pub transform: Transform,
    pub near: f32,
    pub far: f32,
}

/// A map chunk (`pc/data/maps/<area>/<name>.img` + its loose `.wpl`), before any models load.
pub struct ChunkPlan {
    pub name: String,
    /// High-detail objects (streamed in when the camera is near).
    pub hd: Vec<Placed>,
    /// Low-detail stand-ins (always loaded).
    pub lods: Vec<Placed>,
    pub center: Vec3,
    pub radius: f32,
    /// Archives whose .wdd dictionaries may hold this chunk's models.
    pub dict_archives: Vec<usize>,
    /// The chunk's own archive (its .wbn collision lives there).
    pub archive: usize,
}

/// All of a chunk's .wbn collision merged into one triangle soup, in Bevy space.
pub fn load_collision(index: &GameIndex, archive: usize) -> Result<crate::collision::ChunkCollision> {
    let img = &index.imgs[archive];
    let (mut verts, mut tris) = (Vec::new(), Vec::new());
    for (e, entry) in img.entries.iter().enumerate() {
        if entry.ext() != "wbn" {
            continue;
        }
        let m = rage::bounds::read_wbn(&index.resource((archive, e))?)
            .with_context(|| format!("collision {}", entry.name))?;
        let base = verts.len() as u32;
        verts.extend(m.vertices.iter().map(|&v| assets::to_bevy(v)));
        tris.extend(m.triangles.iter().map(|t| t.map(|i| i + base)));
    }
    Ok(crate::collision::ChunkCollision::new(&verts, &tris))
}

/// GTA → Bevy placement. Positions: (x, y, z) → (x, z, -y). WPL rotations are stored inverted.
pub fn placement_transform(i: &Instance) -> Transform {
    let [x, y, z] = i.position;
    let [qx, qy, qz, qw] = i.rotation;
    let q = Quat::from_xyzw(qx, qz, -qy, qw).normalize().conjugate();
    Transform { translation: Vec3::new(x, z, -y), rotation: q, ..default() }
}

/// Chunk names matching a comma list with optional trailing `*` (e.g. `manhat*,nj_01`), or
/// `city` for every chunk of the three islands.
pub fn chunk_names(index: &GameIndex, pattern: &str) -> Vec<String> {
    let all: Vec<String> = index
        .imgs
        .iter()
        .filter_map(|img| {
            let p = img.path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
            let area = ["/maps/manhat/", "/maps/east/", "/maps/jersey/"].iter().any(|a| p.contains(a));
            // a chunk has placements beside it
            (area && img.path.with_extension("wpl").exists())
                .then(|| img.path.file_stem().unwrap().to_string_lossy().to_ascii_lowercase())
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    for pat in pattern.split(',').map(|p| p.trim().to_ascii_lowercase()) {
        for n in &all {
            let hit = pat == "city"
                || pat == *n
                || pat.strip_suffix('*').is_some_and(|pre| n.starts_with(pre));
            if hit && !out.contains(n) {
                out.push(n.clone());
            }
        }
    }
    out
}

pub fn plan_chunk(index: &GameIndex, name: &str) -> Result<ChunkPlan> {
    let a = index.archive(&format!("/{name}.img")).with_context(|| format!("no archive {name}.img"))?;
    let img = &index.imgs[a];
    let draw = |h: u32| index.model_draw_dist.get(&h).copied().unwrap_or(150.0);

    let mut streamed = Vec::new();
    for e in img.entries.iter().filter(|e| e.ext() == "wpl") {
        streamed.extend(placement::read_wpl(&img.read(e)?)?);
    }
    let loose_path = img.path.with_extension("wpl");
    let loose = if loose_path.exists() { placement::read_wpl(&std::fs::read(&loose_path)?)? } else { Vec::new() };

    // An entry of the loose file is a stand-in iff something's `lod` points at it.
    let mut switch: HashMap<usize, f32> = HashMap::new();
    for i in streamed.iter().chain(loose.iter()) {
        if i.lod >= 0 && (i.lod as usize) < loose.len() {
            let s = switch.entry(i.lod as usize).or_insert(0.0);
            *s = s.max(draw(i.model_hash));
        }
    }
    let hd_far = |i: &Instance| match i.lod {
        l if l >= 0 => switch.get(&(l as usize)).copied().unwrap_or_else(|| draw(i.model_hash)),
        _ => draw(i.model_hash),
    };
    let place = |i: &Instance, near: f32, far: f32| Placed {
        model: i.model_hash,
        transform: placement_transform(i),
        near,
        far,
    };
    let mut hd: Vec<Placed> = streamed.iter().map(|i| place(i, 0.0, hd_far(i))).collect();
    let mut lods = Vec::new();
    for (n, i) in loose.iter().enumerate() {
        match switch.get(&n) {
            // a stand-in that is itself replaced by a closer stand-in keeps the same rule
            Some(&s) => lods.push(place(i, s, draw(i.model_hash).max(s * 2.5))),
            None => hd.push(place(i, 0.0, hd_far(i))),
        }
    }

    // LIBERTY_DUMP_PLACEMENTS=<dir>: one CSV per chunk, every instance as the file has it and as
    // we place it (mapped back to GTA axes), for re/viview_diff.py
    if let Ok(dir) = std::env::var("LIBERTY_DUMP_PLACEMENTS") {
        let mut out = String::from(
            "chunk,src,idx,model,name,kind,near,far,flags,lod,raw_x,raw_y,raw_z,raw_qx,raw_qy,raw_qz,raw_qw,x,y,z,qx,qy,qz,qw\n",
        );
        for (src, list) in [("streamed", &streamed), ("loose", &loose)] {
            for (n, i) in list.iter().enumerate() {
                let (kind, near, far) = match switch.get(&n).filter(|_| src == "loose") {
                    Some(&s) => ("lod", s, draw(i.model_hash).max(s * 2.5)),
                    None => ("hd", 0.0, hd_far(i)),
                };
                let t = placement_transform(i);
                let (p, q) = (t.translation, t.rotation);
                let [rx, ry, rz] = i.position;
                let [rqx, rqy, rqz, rqw] = i.rotation;
                let model = index.model_name.get(&i.model_hash).map(String::as_str).unwrap_or("");
                out += &format!(
                    "{name},{src},{n},{:08x},{model},{kind},{near},{far},{:#x},{},{rx},{ry},{rz},{rqx},{rqy},{rqz},{rqw},{},{},{},{},{},{},{}\n",
                    i.model_hash, i.flags, i.lod, p.x, -p.z, p.y, q.x, -q.z, q.y, q.w
                );
            }
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.csv")), out)?;
    }

    let pts: Vec<Vec3> = hd.iter().chain(lods.iter()).map(|p| p.transform.translation).collect();
    let (lo, hi) = pts.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let center = if pts.is_empty() { Vec3::ZERO } else { (lo + hi) / 2.0 };
    let radius = pts.iter().map(|p| (*p - center).xz().length()).fold(0.0, f32::max);

    // .wdd dictionaries: this chunk's own, shared props, and the cross-reference (xr/xref) archives.
    let dict_archives = (0..index.imgs.len())
        .filter(|&i| {
            let p = index.imgs[i].path.to_string_lossy().to_ascii_lowercase();
            i == a || p.contains("props") || p.contains("xr")
        })
        .collect();
    Ok(ChunkPlan { name: name.to_string(), hd, lods, center, radius, dict_archives, archive: a })
}

/// One model turned into uploadable pieces: (mesh, its textures, alpha mode, glow).
pub struct BuiltModel {
    pub parts: Vec<(Mesh, MatKey, AlphaMode, assets::Glow)>,
    pub lights: Vec<rage::drawable::Light>,
}

/// Everything a batch of models needs, built off the main thread.
pub struct Built {
    pub models: Vec<(u32, BuiltModel)>,
    pub images: Vec<(ImageKey, Image)>,
    pub missing_models: usize,
}

/// Load + build `hashes` (skipping any in `skip`): drawables, their texture packs (with
/// parents), meshes and GPU images. Runs on a background thread.
pub fn build_models(
    index: &GameIndex,
    hashes: &[u32],
    dict_archives: &[usize],
    have_images: &HashSet<ImageKey>,
) -> Result<Built> {
    let drawables: HashMap<u32, Drawable> = index.load_models(hashes, dict_archives)?;

    // Texture packs: each model's pack and its parents, loaded once per batch.
    let mut packs: HashMap<String, HashMap<String, Texture>> = HashMap::new();
    let mut chains: HashMap<u32, Vec<String>> = HashMap::new();
    for h in drawables.keys() {
        let chain = index.model_txd.get(h).map(|t| index.txd_chain(t)).unwrap_or_default();
        for p in &chain {
            if !packs.contains_key(p) {
                let tex = index.load_txd(p)?.unwrap_or_default();
                packs.insert(p.clone(), tex.into_iter().map(|t| (t.name.to_ascii_lowercase(), t)).collect());
            }
        }
        chains.insert(*h, chain);
    }

    let mut images: HashMap<ImageKey, Image> = HashMap::new();
    let mut models = Vec::with_capacity(drawables.len());
    for (h, d) in &drawables {
        let embedded: HashMap<String, &Texture> =
            d.textures.iter().map(|t| (t.name.to_ascii_lowercase(), t)).collect();
        let chain = &chains[h];
        // A texture by name: embedded in the model, else along its pack chain. Normal maps are
        // uploaded as data (linear), colour and specular maps as sRGB.
        let mut find = |name: Option<&String>, linear: bool| -> Option<(ImageKey, Format)> {
            let name = name?;
            let upload = |t: &Texture| if linear { assets::gpu_image_linear(t) } else { assets::gpu_image(t) };
            if let Some(t) = embedded.get(name) {
                let key = ImageKey { pack: format!("@{h:08x}"), name: name.clone() };
                if !have_images.contains(&key) {
                    images.entry(key.clone()).or_insert_with(|| upload(t));
                }
                return Some((key, t.format));
            }
            chain.iter().find_map(|pack| {
                let t = packs.get(pack)?.get(name)?;
                let key = ImageKey { pack: pack.clone(), name: name.clone() };
                if !have_images.contains(&key) {
                    images.entry(key.clone()).or_insert_with(|| upload(t));
                }
                Some((key, t.format))
            })
        };
        // Per shader: its textures, and the diffuse texture's format (for the alpha mode).
        let shader_tex: Vec<(MatKey, Option<Format>)> = d
            .shaders
            .iter()
            .map(|s| {
                let diffuse = find(s.diffuse.as_ref(), false);
                let normal = find(s.normal.as_ref(), true).map(|(k, _)| k);
                let specular = find(s.specular.as_ref(), false).map(|(k, _)| k);
                (MatKey { diffuse: diffuse.as_ref().map(|(k, _)| k.clone()), normal, specular }, diffuse.map(|(_, f)| f))
            })
            .collect();
        let parts = d
            .meshes
            .iter()
            .map(|m| {
                let (key, format) = shader_tex.get(m.shader).cloned().unwrap_or_default();
                let alpha = match format {
                    Some(Format::Dxt3 | Format::Dxt5) => AlphaMode::Mask(0.5),
                    _ => AlphaMode::Opaque,
                };
                let glow = d.shaders.get(m.shader).map_or(assets::Glow::None, |s| assets::Glow::from_shader(&s.name, s.emissive));
                if glow != assets::Glow::None && std::env::var("LIBERTY_GLOW_DEBUG").is_ok() && !m.colors.is_empty() {
                    let n = m.colors.len() as f32;
                    let mean = m.colors.iter().fold([0.0f32; 4], |a, c| [a[0] + c[0] as f32, a[1] + c[1] as f32, a[2] + c[2] as f32, a[3] + c[3] as f32]);
                    info!("glowmesh {glow:?} vertex colour mean {:.0} {:.0} {:.0} {:.0}", mean[0] / n, mean[1] / n, mean[2] / n, mean[3] / n);
                }
                (assets::build_mesh(m), key, alpha, glow)
            })
            .collect();
        models.push((*h, BuiltModel { parts, lights: d.lights.clone() }));
    }
    Ok(Built {
        missing_models: hashes.len() - drawables.len(),
        models,
        images: images.into_iter().collect(),
    })
}
