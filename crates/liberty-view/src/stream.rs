//! Streaming the city: stand-ins for every chunk load first; high-detail chunks load on a
//! background thread when the camera comes near and unload when it leaves. Meshes, materials
//! and textures are shared across chunks and reference-counted, so unloading frees memory.
//! Every placed object shows only inside its own distance band (see `world::Placed`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use bevy::prelude::*;
use bevy::tasks::futures_lite::future;
use bevy::tasks::{AsyncComputeTaskPool, Task};
use rage::index::GameIndex;

use crate::assets::{self, ImageKey, MatKey};
use crate::collision::ChunkCollision;
use crate::world::{self, Built, ChunkPlan, Placed};

/// High-detail chunks load when the camera is this close to their edge (× view distance)…
const LOAD_MARGIN: f32 = 320.0;
/// …and unload beyond this (hysteresis so they don't thrash).
const UNLOAD_MARGIN: f32 = 520.0;

#[derive(PartialEq)]
enum ChunkState {
    Unloaded,
    Loading,
    Loaded,
}

enum Job {
    Lods,
    Chunk(usize),
}

#[derive(Resource)]
pub struct City {
    pub index: Arc<GameIndex>,
    pub chunks: Vec<ChunkPlan>,
    state: Vec<ChunkState>,
    roots: Vec<Vec<Entity>>,
    lods_ready: bool,
    task: Option<(Job, Task<Result<(Built, Option<ChunkCollision>)>>)>,
    /// Collision of every loaded HD chunk (Bevy space).
    pub collision: HashMap<usize, ChunkCollision>,
    /// GTA's "view distance" slider.
    pub view: f32,
    pub missing_models: usize,
}

impl City {
    pub fn new(index: GameIndex, chunks: Vec<ChunkPlan>) -> City {
        let n = chunks.len();
        City {
            index: Arc::new(index),
            chunks,
            state: (0..n).map(|_| ChunkState::Unloaded).collect(),
            roots: vec![Vec::new(); n],
            lods_ready: false,
            task: None,
            view: std::env::var("LIBERTY_VIEW").ok().and_then(|v| v.parse::<f32>().ok()).map_or(1.0, |v| v.clamp(0.3, 4.0)),
            missing_models: 0,
            collision: HashMap::new(),
        }
    }

    pub fn loaded(&self) -> usize {
        self.state.iter().filter(|s| **s == ChunkState::Loaded).count()
    }

    /// Nothing loading and nothing in range waiting to load.
    pub fn idle(&self, cam: Vec3) -> bool {
        self.lods_ready && self.task.is_none() && self.next_to_load(cam).is_none()
    }

    fn edge_distance(&self, c: usize, cam: Vec3) -> f32 {
        ((cam - self.chunks[c].center).xz().length() - self.chunks[c].radius).max(0.0)
    }

    fn next_to_load(&self, cam: Vec3) -> Option<usize> {
        (0..self.chunks.len())
            .filter(|&c| self.state[c] == ChunkState::Unloaded)
            .map(|c| (c, self.edge_distance(c, cam)))
            .filter(|&(_, d)| d < LOAD_MARGIN * self.view)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(c, _)| c)
    }
}

/// Shared GPU assets with reference counts (one count per chunk / stand-in set using them).
#[derive(Resource, Default)]
pub struct Cache {
    models: HashMap<u32, CachedModel>,
    images: HashMap<ImageKey, (Handle<Image>, u32)>,
    materials: HashMap<(MatKey, bool, assets::Glow), Handle<StandardMaterial>>,
    /// Self-lit materials, for the time of day to turn up or down.
    pub glow: Vec<(AssetId<StandardMaterial>, assets::Glow)>,
    /// Textures released since the last `forget_released` (their materials go then, in one pass).
    released: HashSet<ImageKey>,
}

struct CachedModel {
    parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    lights: Vec<rage::drawable::Light>,
    images: Vec<ImageKey>,
    refs: u32,
}

impl Cache {
    fn retain(&mut self, model: u32) {
        let Some(m) = self.models.get_mut(&model) else { return };
        m.refs += 1;
        if m.refs == 1 {
            for k in m.images.clone() {
                if let Some(i) = self.images.get_mut(&k) {
                    i.1 += 1;
                }
            }
        }
    }

    fn release(&mut self, model: u32) {
        let Some(m) = self.models.get_mut(&model) else { return };
        m.refs = m.refs.saturating_sub(1);
        if m.refs > 0 {
            return;
        }
        let m = self.models.remove(&model).unwrap();
        for k in m.images {
            let gone = self.images.get_mut(&k).is_some_and(|i| {
                i.1 = i.1.saturating_sub(1);
                i.1 == 0
            });
            if gone {
                self.images.remove(&k);
                self.released.insert(k);
            }
        }
        // dropping the handles lets Bevy free the meshes and (now unreferenced) textures
    }

    /// Drop the materials that use a released texture, and those materials from the glow list.
    /// Done once after an unload, not per texture: per texture it scanned every material (and the
    /// glow list against every material) hundreds of times over, which froze the game for 30-80 s
    /// when a dense chunk unloaded (flying into Algonquin, 2 Oct).
    fn forget_released(&mut self) {
        if self.released.is_empty() {
            return;
        }
        let gone = std::mem::take(&mut self.released);
        let uses = |k: &Option<ImageKey>| k.as_ref().is_some_and(|k| gone.contains(k));
        self.materials.retain(|(key, _, _), _| !(uses(&key.diffuse) || uses(&key.normal) || uses(&key.specular)));
        let live: HashSet<AssetId<StandardMaterial>> = self.materials.values().map(|h| h.id()).collect();
        self.glow.retain(|(id, _)| live.contains(id));
    }
}

/// Marks a placed object's root entity with its distance band.
#[derive(Component)]
pub struct Band {
    near: f32,
    far: f32,
    /// Stand-ins: which chunk they stand in for (shown up close while that chunk isn't loaded).
    stands_in_for: Option<usize>,
}

fn spawn_task(city: &mut City, cache: &Cache, job: Job) {
    let hashes: Vec<u32> = match job {
        Job::Lods => city.chunks.iter().flat_map(|c| c.lods.iter().map(|p| p.model)).collect(),
        Job::Chunk(c) => city.chunks[c].hd.iter().map(|p| p.model).collect(),
    };
    let hashes: Vec<u32> =
        hashes.into_iter().collect::<HashSet<_>>().into_iter().filter(|h| !cache.models.contains_key(h)).collect();
    let dicts: Vec<usize> = match job {
        Job::Lods => city.chunks.iter().flat_map(|c| c.dict_archives.iter().copied()).collect::<HashSet<_>>().into_iter().collect(),
        Job::Chunk(c) => city.chunks[c].dict_archives.clone(),
    };
    let have: HashSet<ImageKey> = cache.images.keys().cloned().collect();
    let index = city.index.clone();
    let archive = match job {
        Job::Chunk(c) => Some(city.chunks[c].archive),
        Job::Lods => None,
    };
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let built = world::build_models(&index, &hashes, &dicts, &have)?;
        let collision = archive.map(|a| world::load_collision(&index, a)).transpose()?;
        Ok((built, collision))
    });
    if let Job::Chunk(c) = job {
        city.state[c] = ChunkState::Loading;
    }
    city.task = Some((job, task));
}

/// Upload a finished batch into the cache (refcount 0; spawning retains).
fn integrate(
    built: Built,
    cache: &mut Cache,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    for (key, image) in built.images {
        cache.images.entry(key).or_insert_with(|| (images.add(image), 0));
    }
    for (hash, model) in built.models {
        if cache.models.contains_key(&hash) {
            continue;
        }
        let mut used = Vec::new();
        let lights = model.lights;
        let parts = model
            .parts
            .into_iter()
            .map(|(mesh, key, alpha, glow)| {
                let masked = matches!(alpha, AlphaMode::Mask(_));
                let handle = |k: &Option<ImageKey>| k.as_ref().and_then(|k| cache.images.get(k)).map(|(h, _)| h.clone());
                let (tex, normal, specular) = (handle(&key.diffuse), handle(&key.normal), handle(&key.specular));
                for k in [&key.diffuse, &key.normal, &key.specular].into_iter().flatten() {
                    if !used.contains(k) {
                        used.push(k.clone());
                    }
                }
                let glow = if tex.is_some() { glow } else { assets::Glow::None };
                let key = MatKey {
                    diffuse: key.diffuse.filter(|_| tex.is_some()),
                    normal: key.normal.filter(|_| normal.is_some()),
                    specular: key.specular.filter(|_| specular.is_some()),
                };
                let mut fresh = false;
                let mat = cache
                    .materials
                    .entry((key, masked, glow))
                    .or_insert_with(|| {
                        fresh = true;
                        let mut m = assets::material(tex.clone(), alpha);
                        assets::apply_maps(&mut m, normal.clone(), specular.clone());
                        if glow != assets::Glow::None {
                            // lit by its own texture; strength is set by the time of day
                            m.emissive_texture = tex.clone();
                            m.emissive = LinearRgba::BLACK;
                        }
                        materials.add(m)
                    })
                    .clone();
                if fresh && glow != assets::Glow::None {
                    cache.glow.push((mat.id(), glow));
                }
                (meshes.add(mesh), mat)
            })
            .collect();
        cache.models.insert(hash, CachedModel { parts, lights, images: used, refs: 0 });
    }
}

fn spawn_placed(commands: &mut Commands, cache: &mut Cache, p: &Placed, stands_in_for: Option<usize>) -> Option<Entity> {
    let parts = cache.models.get(&p.model)?.parts.clone();
    let lights = cache.models.get(&p.model)?.lights.clone();
    cache.retain(p.model);
    let e = commands
        .spawn((p.transform, Visibility::Hidden, Band { near: p.near, far: p.far, stands_in_for }))
        .with_children(|c| {
            for (mesh, mat) in parts {
                c.spawn((Mesh3d(mesh), MeshMaterial3d(mat)));
            }
        })
        .id();
    // the model's own lights (lights.rs turns the nearest ones on after dark). The far stand-in
    // models carry lights too, and the game lights those even from close by: its run-time light
    // list at Star Junction (session, 03:18, `re/dump_lights.py`) holds the road stand-in's fill
    // light (range 80, strength 15) and not the close-up road's (range 40, strength 12) at the
    // same spot. lights.rs keeps the wider one where two sit together.
    // traffic signals are lights too (pure green, amber and red, three to a head): they are not
    // lamps: lights::signals lights the one the traffic-light cycle says
    let (signals, lights): (Vec<_>, Vec<_>) = lights.into_iter().partition(|l| crate::lights::signal_colour(l.color).is_some());
    if !signals.is_empty() && stands_in_for.is_none() {
        commands.entity(e).insert(crate::lights::Signals { lights: signals, phases: Vec::new() });
    }
    if !lights.is_empty() {
        commands.entity(e).insert(crate::lights::Lamps(lights));
    }
    Some(e)
}

#[allow(clippy::too_many_arguments)]
pub fn stream(
    mut commands: Commands,
    mut city: ResMut<City>,
    mut cache: ResMut<Cache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    cam: Query<&Transform, With<Camera3d>>,
) {
    let _slow = crate::SlowGuard::new("stream::stream");
    let Ok(cam) = cam.single() else { return };
    let cam = cam.translation;

    // 1. finished job → upload + spawn
    let done = city.task.as_mut().and_then(|(_, t)| future::block_on(future::poll_once(t)));
    if let Some(result) = done {
        let (job, _) = city.task.take().unwrap();
        match result {
            Ok((built, collision)) => {
                city.missing_models += built.missing_models;
                integrate(built, &mut cache, &mut meshes, &mut materials, &mut images);
                match job {
                    Job::Lods => {
                        for c in 0..city.chunks.len() {
                            for p in city.chunks[c].lods.clone() {
                                spawn_placed(&mut commands, &mut cache, &p, Some(c));
                            }
                        }
                        city.lods_ready = true;
                    }
                    Job::Chunk(c) => {
                        let roots: Vec<Entity> = city.chunks[c]
                            .hd
                            .clone()
                            .iter()
                            .filter_map(|p| spawn_placed(&mut commands, &mut cache, p, None))
                            .collect();
                        info!("loaded {} ({} objects, {} collision triangles)", city.chunks[c].name, roots.len(), collision.as_ref().map_or(0, |k| k.triangle_count()));
                        if let Some(k) = collision {
                            city.collision.insert(c, k);
                        }
                        city.roots[c] = roots;
                        city.state[c] = ChunkState::Loaded;
                    }
                }
            }
            Err(e) => {
                error!("load failed: {e:#}");
                if let Job::Chunk(c) = job {
                    city.state[c] = ChunkState::Loaded; // don't retry forever
                }
            }
        }
    }

    // 2. unload far chunks
    let view = city.view;
    for c in 0..city.chunks.len() {
        if city.state[c] == ChunkState::Loaded && city.edge_distance(c, cam) > UNLOAD_MARGIN * view {
            for e in std::mem::take(&mut city.roots[c]) {
                commands.entity(e).despawn();
            }
            for p in &city.chunks[c].hd {
                cache.release(p.model);
            }
            city.collision.remove(&c);
            city.state[c] = ChunkState::Unloaded;
        }
    }
    cache.forget_released();

    // 3. start the next job (stand-ins first, then the nearest chunk in range)
    if city.task.is_none() {
        if !city.lods_ready {
            spawn_task(&mut city, &cache, Job::Lods);
        } else if let Some(c) = city.next_to_load(cam) {
            spawn_task(&mut city, &cache, Job::Chunk(c));
        }
    }
}

/// Show each object only inside its distance band; stand-ins also show up close while the
/// chunk they stand in for isn't loaded.
pub fn distance_bands(city: Res<City>, cam: Query<&Transform, With<Camera3d>>, mut q: Query<(&Transform, &Band, &mut Visibility)>) {
    let _slow = crate::SlowGuard::new("stream::distance_bands");
    let Ok(cam) = cam.single() else { return };
    let loaded: Vec<bool> = city.state.iter().map(|s| *s == ChunkState::Loaded).collect();
    for (t, band, mut vis) in &mut q {
        let d = t.translation.distance(cam.translation);
        let near = match band.stands_in_for {
            Some(c) if !loaded[c] => 0.0,
            _ => band.near * city.view,
        };
        let want = if d >= near && d < band.far * city.view { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
}
