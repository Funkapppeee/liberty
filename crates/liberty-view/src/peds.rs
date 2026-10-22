//! Pedestrians: component peds from `componentpeds.img`, dressed by `pedVariations.dat`, moving
//! with their own `peds.ide` movement group (`move_m@casual`, `move_gng@afro_c`, …), spawned around
//! the camera. They spawn on the navmesh's pavement polygons (`rage::navmesh`: the game's own
//! map of where peds walk) and wander the way IV's do (exe: CTaskComplexMoveWander and the
//! wander task 0xcb1750): a heading that follows where they actually walk, a route of 8-20 m of
//! pavement planned along it, walked in straight lines, and roads crossed only at the game's
//! crossings (paths.img's ped nodes), waiting for the walk signal where there's a light.
//! A ped hit by a bullet drops into a ragdoll built from its own `.wft` body, like Niko's, and
//! loses the weapon's damage from 100 health: alive, he gets up (the game's get-up clips) and
//! runs; at 0 he stays down. Peds near a gunshot run from it.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use bevy::prelude::*;
use bevy::render::mesh::skinning::SkinnedMeshInverseBindposes;
use rage::drawable::{self, Bone, Drawable};
use rage::peds::PedVariation;
use rage::wtd::{self, Texture};
use rage::{Img, Key, Resource as Rsc};

use crate::assets::to_bevy;
use crate::collision;
use crate::ped::{HIP_HEIGHT, Helper, Player, build_rig, helper_bones, place_helpers, rig_of};
use crate::weapon::Armed;
use crate::pedanim::{self, Locomotion};
use crate::ragdoll::{CarBox, Mode, RagdollDef, RagdollSim, Tuning};
use crate::stream::City;
use crate::traffic::{self, Roads};
use crate::vehicle::Car;
use crate::weapon::BulletHit;

const WANT_PEDS: usize = 20;
const SPAWN_MIN: f32 = 25.0;
const SPAWN_MAX: f32 = 60.0;
const DESPAWN: f32 = 90.0;

/// Everything loaded for one ped model, shared by every ped of that model.
struct PedModel {
    skeleton: Vec<Bone>,
    /// Component drawables by name hash.
    parts: HashMap<u32, Drawable>,
    textures: HashMap<String, Texture>,
    anims: Arc<Vec<rage::anim::Animation>>,
    /// The ragdoll body from the ped's .wft (21 parts, masses, joint limits).
    rag_def: Option<Arc<RagdollDef>>,
    /// The arm skin helper bones (forearm twist, upper-arm roll) and their rules, as for Niko.
    helpers: Arc<Vec<Helper>>,
}

#[derive(Resource)]
pub struct PedPool {
    peds_img: Img,
    anim_img: Img,
    /// Spawnable models (lowercase) with their variations and movement group.
    models: Vec<(String, PedVariation, String)>,
    loaded: HashMap<String, Arc<PedModel>>,
    group_anims: HashMap<String, Arc<Vec<rage::anim::Animation>>>,
    fallback: HashMap<String, String>,
    rng: u64,
    cooldown: f32,
    /// Euphoria's joint frames and limits (Fred.art), shared by every ped's ragdoll.
    nm_rig: rage::nmart::Biped,
    /// Getting up after a fall (get_up.wad face down, get_up_back.wad on the back), for every ped.
    getups: Vec<rage::anim::Animation>,
}

impl PedPool {
    pub fn open(root: &Path, key: &Key) -> Result<PedPool> {
        let data = root.join("common/data");
        let variations = rage::peds::read_ped_variations(&std::fs::read_to_string(data.join("pedVariations.dat"))?)?;
        let defs = rage::peds::read_peds_ide(&std::fs::read_to_string(data.join("peds.ide"))?)?;
        let fallback = rage::peds::read_moveblend(&std::fs::read_to_string(data.join("moveblend.dat"))?)?;
        let peds_img = Img::open(&root.join("pc/models/cdimages/componentpeds.img"), key)?;
        let anim_img = Img::open(&root.join("pc/anim/anim.img"), key)?;
        let mut models = Vec::new();
        for d in &defs {
            let m = d.model.to_ascii_lowercase();
            // street peds only: skip the player/multiplayer/cutscene-only models
            if m.contains("multiplayer") || m.starts_with("ig_") || m.starts_with("cs_") || m == "player" {
                continue;
            }
            if let (Some(v), Some(_)) = (variations.get(&m), peds_img.find(&format!("{m}.wdd"))) {
                models.push((m, v.clone(), d.move_group.to_ascii_lowercase()));
            }
        }
        info!("peds: {} spawnable models", models.len());
        // same naming as Niko's: getup_front_<clip> / getup_back_<clip>
        let mut getups = Vec::new();
        for (pack, prefix) in [("get_up.wad", "getup_front_"), ("get_up_back.wad", "getup_back_")] {
            if let Some(e) = anim_img.find(pack) {
                for mut a in rage::anim::read_dictionary(&Rsc::load(&anim_img.read(e)?)?)? {
                    a.name = format!("{prefix}{}", a.name.trim_start_matches("get_up_"));
                    getups.push(a);
                }
            }
        }
        Ok(PedPool {
            peds_img,
            anim_img,
            models,
            loaded: HashMap::new(),
            group_anims: HashMap::new(),
            fallback,
            rng: 0x9E37_79B9_7F4A_7C15,
            cooldown: 0.0,
            nm_rig: std::fs::read(root.join("pc/data/naturalmotion/Fred.art")).ok().and_then(|d| rage::nmart::read_biped_rig(&d).ok()).unwrap_or_default(),
            getups,
        })
    }

    fn rand(&mut self) -> f32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        ((self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32) / (1u64 << 24) as f32
    }

    fn pick(&mut self, n: usize) -> usize {
        ((self.rand() * n as f32) as usize).min(n.saturating_sub(1))
    }

    /// The movement group's clips, following moveblend.dat fallbacks until one has idle+walk.
    fn anims_for(&mut self, group: &str) -> Result<Arc<Vec<rage::anim::Animation>>> {
        let mut g = group.to_string();
        for _ in 0..6 {
            if let Some(a) = self.group_anims.get(&g) {
                return Ok(a.clone());
            }
            if let Some(e) = self.anim_img.find(&format!("{g}.wad")) {
                let anims = rage::anim::read_dictionary(&Rsc::load(&self.anim_img.read(e)?)?)?;
                if ["idle", "walk"].iter().all(|n| anims.iter().any(|a| a.name == *n)) {
                    let a = Arc::new(anims);
                    self.group_anims.insert(g, a.clone());
                    return Ok(a);
                }
            }
            match self.fallback.get(&g) {
                Some(f) if *f != g => g = f.clone(),
                _ => break,
            }
        }
        // last resort: the generic male set
        let e = self.anim_img.find("move_m@casual.wad").context("move_m@casual.wad")?;
        let a = Arc::new(rage::anim::read_dictionary(&Rsc::load(&self.anim_img.read(e)?)?)?);
        self.group_anims.insert(group.to_string(), a.clone());
        Ok(a)
    }

    fn model(&mut self, idx: usize) -> Result<Arc<PedModel>> {
        let (name, _, group) = self.models[idx].clone();
        if let Some(m) = self.loaded.get(&name) {
            return Ok(m.clone());
        }
        let read = |ext: &str| -> Result<Rsc> {
            let e = self.peds_img.find(&format!("{name}.{ext}")).with_context(|| format!("{name}.{ext}"))?;
            Rsc::load(&self.peds_img.read(e)?)
        };
        let wft = read("wft")?;
        let skeleton = drawable::read_skeleton(&wft, wft.u32(Rsc::ROOT, 0xB4)?)?;
        let parts: HashMap<u32, Drawable> = drawable::read_dictionary(&read("wdd")?)?.into_iter().collect();
        let textures = wtd::read(&read("wtd")?)?.into_iter().map(|t| (t.name.to_ascii_lowercase(), t)).collect();
        let anims = self.anims_for(&group)?;
        let rag_def = rage::ragdoll::read_ragdoll(&wft).ok().map(|body| Arc::new(RagdollDef::new(&skeleton, &body, &self.nm_rig)));
        let helpers = Arc::new(helper_bones(&skeleton));
        let m = Arc::new(PedModel { skeleton, parts, textures, anims, rag_def, helpers });
        self.loaded.insert(name, m.clone());
        Ok(m)
    }
}

#[derive(Component)]
pub struct Npc {
    yaw: f32,
    speed: f32,
    phase: f32,
    idle_phase: f32,
    /// Seconds left of the current wander decision (walking or standing).
    timer: f32,
    walking: bool,
    loco: Option<Locomotion>,
    joints: Vec<(Entity, Transform, Quat)>,
    /// The pose he was last drawn in (what a ragdoll starts from).
    last_pose: Vec<(Quat, Vec3)>,
    rag_def: Option<Arc<RagdollDef>>,
    helpers: Arc<Vec<Helper>>,
    /// Knocked down: the running ragdoll.
    rag: Option<RagdollSim>,
    /// 100 at spawn, less each bullet's damage (WeaponInfo.xml); at 0 he doesn't get up.
    health: f32,
    /// Getting up: the clip, seconds into it, and where and which way it started.
    getup: Option<(String, f32, Vec3, f32)>,
    /// Back from a stagger he stayed up through: the ragdoll's last pose, faded out over the
    /// seconds left.
    fade: Option<(Vec<(Quat, Vec3)>, f32)>,
    /// Seconds left of running away, and what from.
    panic: f32,
    flee_from: Vec3,
    /// The pavement polygon he's on (area id, polygon), the one he came from, and the way ahead:
    /// the polygon edges he'll cross (Bevy space) with the polygon each leads into.
    at: Option<(u16, u16)>,
    came: Option<(u16, u16)>,
    route: VecDeque<Portal>,
    /// Where the planned way ends (a point just past its last edge).
    goal: Option<Vec3>,
    /// The way he means to go (yaw, as `yaw`), and seconds until it next follows his facing.
    wander: f32,
    wander_at: f32,
    /// Off the pavement: the pavement polygon he's walking back to, and its middle.
    rejoin: Option<((u16, u16), Vec3)>,
    /// Seconds until he next looks for pavement under him (after running, or spawned off it).
    look: f32,
    /// Crossing a road at one of the game's crossings, and the timers around it.
    cross: Option<Crossing>,
    cross_check: f32,
    cross_cool: f32,
    /// Fleeing and blocked by a drop (a quay edge): the heading he runs along it instead, and for
    /// how long.
    detour: Option<(f32, f32)>,
}

impl Npc {
    /// Knocked down or getting up (not a standing target).
    pub fn is_down(&self) -> bool {
        self.rag.is_some() || self.getup.is_some()
    }

    /// For the test lab (testlab.rs): the running ragdoll and its body.
    pub fn lab_rag(&self) -> Option<(&RagdollSim, &RagdollDef)> {
        self.rag.as_ref().zip(self.rag_def.as_deref())
    }

    /// For tests: health left, and what he's doing.
    pub fn state(&self) -> String {
        let doing = if self.rag.is_some() {
            "down"
        } else if self.getup.is_some() {
            "getting up"
        } else if self.panic > 0.0 {
            "fleeing"
        } else if let Some(c) = &self.cross {
            ["going to a crossing", "waiting to cross", "crossing the road"][c.stage as usize]
        } else if self.at.is_some() {
            "walking the pavement"
        } else {
            "walking"
        };
        format!("health {:.0}, {doing}", self.health)
    }
}

/// Peds on or off (the dev panel's World switch). Off: no new peds and the ones walking go.
/// Starts off with `LIBERTY_NO_PEDS`.
#[derive(Resource)]
pub struct PedsOn(pub bool);

impl Default for PedsOn {
    fn default() -> Self {
        PedsOn(std::env::var("LIBERTY_NO_PEDS").is_err())
    }
}

/// Keep ~WANT_PEDS peds near the camera: spawn on real ground, despawn far ones.
#[allow(clippy::too_many_arguments)]
pub fn spawn_peds(
    mut commands: Commands,
    on: Res<PedsOn>,
    time: Res<Time>,
    mut pool: ResMut<PedPool>,
    city: Res<City>,
    cam: Query<&Transform, With<Camera3d>>,
    npcs: Query<(Entity, &Transform), With<Npc>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut cam_still: Local<(Vec3, Quat, u32)>,
    mut nav: ResMut<Nav>,
) {
    let _slow = crate::SlowGuard::new("peds::spawn_peds");
    // LIBERTY_TARGET_PED=<metres>: one ped standing that far ahead of the camera (works on the test pad);
    // LIBERTY_TARGET_WALK: he walks toward the camera instead
    let target_dist: Option<f32> = std::env::var("LIBERTY_TARGET_PED").ok().map(|v| v.parse().unwrap_or(7.0));
    if !on.0 {
        // switched off: the ones walking go (joints and the rest are children)
        for (e, _) in &npcs {
            commands.entity(e).despawn();
        }
    }
    if !on.0 || (crate::testpad::enabled() && target_dist.is_none()) || pool.models.is_empty() {
        if pool.models.is_empty() {
            warn!("no spawnable ped models");
        }
        return;
    }
    let Ok(cam) = cam.single() else { return };
    let mut here = cam.translation;
    // LIBERTY_PED_OVERHEAD=1 (tests: a camera high over a junction): spawn around the ground under
    // the camera, 8-45 m out, so they walk in view
    let overhead = std::env::var_os("LIBERTY_PED_OVERHEAD").is_some();
    if overhead {
        if let Some(g) = collision::ground_height(city.collision.values(), here.x, here.z, here.y) {
            here.y = g;
        }
    }
    let (spawn_min, spawn_max) = if overhead { (8.0, 45.0) } else { (SPAWN_MIN, SPAWN_MAX) };
    // the test target waits for the camera to settle (it lines up with where the camera looks).
    // Settled = within 2 cm / 0.3 deg of where it stopped: the follow camera keeps drifting a few
    // mm with Niko's idle (at pitch -0.05 on the bare pad a 1 mm test never passed, no target)
    if cam_still.0.distance(here) < 0.02 && cam_still.1.angle_between(cam.rotation) < 0.005 {
        cam_still.2 += 1;
    } else {
        if target_dist.is_some() && cam_still.2 > 0 {
            debug!("target ped: camera moved {:.3} m / {:.4} rad, waiting again", cam_still.0.distance(here), cam_still.1.angle_between(cam.rotation));
        }
        *cam_still = (here, cam.rotation, 0);
    }
    if target_dist.is_some() && cam_still.2 < 30 {
        return;
    }
    let mut count = 0;
    for (e, t) in &npcs {
        if t.translation.xz().distance(here.xz()) > DESPAWN {
            commands.entity(e).despawn();
        } else {
            count += 1;
        }
    }
    pool.cooldown -= time.delta_secs();
    let want = if target_dist.is_some() && crate::testpad::enabled() { 1 } else { WANT_PEDS };
    if count >= want || pool.cooldown > 0.0 || city.collision.is_empty() {
        return;
    }
    pool.cooldown = 0.15; // one at a time: loading a new model hits the disk

    // somewhere walkable around the camera, not inside a building
    let ang = pool.rand() * std::f32::consts::TAU;
    let dist = spawn_min + pool.rand() * (spawn_max - spawn_min);
    let mut spot = here + Vec3::new(ang.cos(), 0.0, ang.sin()) * dist;
    // the test target: straight ahead of the camera, turned to face it; (spot, yaw)
    let target = target_dist.filter(|_| count == 0).map(|d| {
        let f = cam.forward().as_vec3().with_y(0.0).normalize_or(Vec3::NEG_Z);
        crate::testpad::lab_target(here, f).unwrap_or((here + f * d, f.x.atan2(f.z))) // a test lab station puts him on its prop
    });
    if let Some((at, _)) = target {
        spot = at;
    }
    // a pavement polygon in the navmesh tile there, if there is one: he starts on the sidewalk
    // (IV spawns its wandering peds on pavement only; where the tile has pavement but none came up
    // in range, try again next time rather than drop him off it)
    let mut on_pavement = None;
    let mut top = here.y + 10.0;
    let mut has_pavement = false;
    if target.is_none() {
        if let Some(tile) = rage::navmesh::area_at(spot.x, -spot.z).and_then(|a| nav.tile(a)) {
            let pavements: Vec<usize> = tile.polys.iter().enumerate().filter(|(_, p)| p.pavement() && p.corners.len() >= 3).map(|(i, _)| i).collect();
            has_pavement = !pavements.is_empty();
            if !pavements.is_empty() {
                let i = pavements[pool.pick(pavements.len())];
                let c = Vec3::from(to_bevy(tile.polys[i].centre()));
                let d = c.xz().distance(here.xz());
                if (spawn_min..spawn_max + 30.0).contains(&d) {
                    spot = c;
                    top = c.y + 1.0;
                    on_pavement = Some((tile.area, i as u16));
                }
            }
        }
    }
    if has_pavement && on_pavement.is_none() {
        return;
    }
    let Some(g) = collision::ground_height(city.collision.values(), spot.x, spot.z, top) else { return };
    let feet = Vec3::new(spot.x, g, spot.z);
    let pushed = collision::push_out_of_walls(city.collision.values(), feet, 0.35, &[0.5, 1.2]);
    if pushed.distance(feet) > 0.05 || (g - here.y).abs() > 25.0 {
        return;
    }

    let n_models = pool.models.len();
    // LIBERTY_PED_MODEL=<name>: every ped is this model (tests)
    let only = std::env::var("LIBERTY_PED_MODEL").ok().and_then(|m| pool.models.iter().position(|p| p.0.eq_ignore_ascii_case(&m)));
    let idx = only.unwrap_or_else(|| pool.pick(n_models));
    let model = match pool.model(idx) {
        Ok(m) => m,
        Err(err) => {
            warn!("ped {}: {err:#}", pool.models[idx].0);
            pool.models.remove(idx);
            return;
        }
    };
    let (name, variation, _) = pool.models[idx].clone();

    // outfit: one geometry per slot, and a texture colour letter per slot
    let mut by_slot: HashMap<&str, Vec<u32>> = HashMap::new();
    for s in variation.slots.iter().filter(|s| !s.is_prop()) {
        by_slot.entry(s.slot.as_str()).or_default().push(s.geometry);
    }
    let mut parts: Vec<&Drawable> = Vec::new();
    let mut letters: HashMap<String, char> = HashMap::new();
    for (slot, geoms) in &by_slot {
        let geom = geoms[pool.pick(geoms.len())];
        let found = rage::peds::drawable_names(slot, geom)
            .iter()
            .find_map(|n| model.parts.get(&rage::placement::hash(n)));
        if let Some(d) = found {
            parts.push(d);
            // colour variants present for this slot+geometry: {slot}_diff_{nnn}_{letter}_…
            let prefix = format!("{slot}_diff_{geom:03}_");
            let mut avail: Vec<char> =
                model.textures.keys().filter_map(|k| k.strip_prefix(&prefix)).filter_map(|r| r.chars().next()).collect();
            avail.sort();
            avail.dedup();
            if !avail.is_empty() {
                letters.insert(slot.to_string(), avail[pool.pick(avail.len())]);
            }
        }
    }
    if parts.is_empty() {
        return;
    }
    let textures = &model.textures;
    let texture = |n: &str| -> Option<&Texture> {
        // swap the variant letter: uppr_diff_000_a_uni → uppr_diff_000_c_uni
        let mut it = n.splitn(5, '_');
        if let (Some(slot), Some("diff"), Some(num), Some(_), Some(rest)) = (it.next(), it.next(), it.next(), it.next(), it.next()) {
            if let Some(l) = letters.get(slot) {
                if let Some(t) = textures.get(&format!("{slot}_diff_{num}_{l}_{rest}")) {
                    return Some(t);
                }
            }
        }
        textures.get(n)
    };
    let (root, joints) = build_rig(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut images,
        &mut bindposes,
        &model.skeleton,
        &parts,
        &texture,
        &name,
    );
    let ids: HashMap<u16, usize> = model.skeleton.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
    // his own movement set plus the shared get-ups
    let mut anims = (*model.anims).clone();
    anims.extend(pool.getups.iter().cloned());
    let loco = Locomotion::from_anims(&anims, &ids, model.skeleton.len());
    let yaw = pool.rand() * std::f32::consts::TAU;
    let phase = pool.rand();
    info!("ped {name} spawned {:.0} m away ({} parts, {} walk)", feet.distance(here), parts.len(), if loco.is_some() { "own" } else { "NO" });
    commands.entity(root).insert((
        Transform::from_translation(feet).with_rotation(Quat::from_rotation_y(yaw)),
        Visibility::Inherited,
        Npc {
            yaw: target.map_or(yaw, |t| t.1),
            speed: 0.0,
            phase,
            idle_phase: phase,
            // a test target stands still, facing the camera
            timer: if target.is_some() { 1.0e6 } else { 0.0 },
            walking: target.is_none() || std::env::var("LIBERTY_TARGET_WALK").is_ok(),
            loco,
            joints,
            last_pose: Vec::new(),
            rag_def: model.rag_def.clone(),
            helpers: model.helpers.clone(),
            rag: None,
            health: 100.0,
            getup: None,
            fade: None,
            panic: 0.0,
            flee_from: Vec3::ZERO,
            at: on_pavement.filter(|_| target.is_none()),
            came: None,
            route: VecDeque::new(),
            goal: None,
            wander: pool.rand() * std::f32::consts::TAU,
            wander_at: 1.0,
            rejoin: None,
            look: 0.0,
            cross: None,
            cross_check: pool.rand() * 2.0,
            cross_cool: 0.0,
            detour: None,
        },
    ));
}

/// Wander: walk a while, drift the heading, turn away from walls, stand around now and then.
pub fn move_peds(
    time: Res<Time>,
    mut pool: ResMut<PedPool>,
    city: Res<City>,
    mut npcs: Query<(Entity, &mut Transform, &mut Npc)>,
    mut bones: Query<&mut Transform, Without<Npc>>,
    cars: Query<(Entity, &Car)>,
    mut shots: EventReader<BulletHit>,
    armed: Option<Res<Armed>>,
    player: Query<&GlobalTransform, With<Player>>,
    mut fired: Local<u32>,
    mut debug_at: Local<f32>,
    mut nav: ResMut<Nav>,
    roads: Option<Res<Roads>>,
) {
    let _slow = crate::SlowGuard::new("peds::move_peds");
    // LIBERTY_PED_DEBUG: twice a second, what every ped is doing
    let debug = std::env::var("LIBERTY_PED_DEBUG").is_ok() && time.elapsed_secs() - *debug_at >= 0.5;
    if debug {
        *debug_at = time.elapsed_secs();
    }
    let dt = time.delta_secs().min(0.05);
    let chunks = city.collision.values();
    let shots: Vec<(Entity, Vec3, f32, f32, Vec3)> = shots.read().filter_map(|h| h.entity.map(|e| (e, h.dir, h.force, h.damage, h.point))).collect();
    // a gunshot: everyone within earshot runs from where it came from
    let shooter = player.single().map(|p| p.translation()).ok();
    let gunfire = armed.as_ref().is_some_and(|a| a.shots != *fired);
    if let Some(a) = &armed {
        *fired = a.shots;
    }
    // the cars, for knocking peds down (moving ones) and for the ragdolls to collide with
    let solids: Vec<CarBox> = cars.iter().map(|(e, c)| c.solid(e.to_bits())).collect();
    for (entity, mut t, mut n) in &mut npcs {
        if debug {
            let g = t.translation;
            info!("ped {entity}: {}, {:.1} m from Niko, speed {:.1}, on {}, at {:.2} {:.2} {:.2}", n.state(), shooter.map_or(0.0, |s| s.distance(g)), n.speed, nav.under(g), g.x, -g.z, g.y);
        }
        if let (true, Some(from)) = (gunfire, shooter) {
            if t.translation.distance(from) < 45.0 && n.health > 0.0 {
                n.panic = 9.0 + pool.rand() * 6.0;
                n.flee_from = from;
            }
        }
        // shot: still alive, IV's shot behaviour (he staggers on IV's balancer, and falls when it
        // gives up); killed, he goes limp from the pose he's in. Either way he's knocked the way
        // the bullet was going. STAND-IN for IV's bullet impulse: its impact force (50 for the
        // pistol) over a nominal 50 gives the push in m/s while he stands, over 25 when he dies.
        if let Some(&(_, dir, force, damage, point)) = shots.iter().find(|s| s.0 == entity) {
            // the shot's headLook point: the shooter's head (IV sends a point near it)
            let shooter_head = shooter.map_or(point - dir * 5.0, |s| s + Vec3::Y * 1.6);
            n.health = (n.health - damage).max(0.0);
            n.getup = None;
            n.fade = None;
            let alive = n.health > 0.0 && std::env::var("LIBERTY_SHOT_LIMP").is_err();
            if let Some(from) = shooter {
                n.panic = 12.0 + pool.rand() * 6.0;
                n.flee_from = from;
            }
            if let Some(def) = n.rag_def.clone() {
                if let Some(sim) = n.rag.as_mut() {
                    if alive {
                        sim.shot_hit(&def, dir * (force / 50.0), point, shooter_head);
                    } else {
                        sim.mode = Mode::Relax;
                        sim.shove(&def, dir * (force / 50.0));
                    }
                } else {
                    let locals: Vec<Transform> = if n.last_pose.len() == n.joints.len() {
                        n.last_pose.iter().map(|(r, p)| Transform { translation: *p, rotation: *r, ..default() }).collect()
                    } else {
                        n.joints.iter().map(|j| j.1).collect()
                    };
                    let fwd = Vec3::new(-n.yaw.sin(), 0.0, -n.yaw.cos());
                    let globals = def.globals(&rig_of(&t), &locals);
                    let mode = if alive { Mode::Balance } else { Mode::Relax };
                    let mut sim = RagdollSim::start(&def, &globals, fwd * n.speed, chunks.clone(), mode, Tuning::default());
                    if alive {
                        sim.shot_hit(&def, dir * (force / 50.0), point, shooter_head);
                    } else {
                        sim.shove(&def, dir * (force / 25.0));
                    }
                    n.rag = Some(sim);
                    n.speed = 0.0;
                }
            }
        }
        // a car about to hit him: the same reactions as Niko's (ped.rs, from IV's taskparams):
        // fast and arriving within braceTargetPredictionTime, hands up (braceForImpact); already on
        // him and fast, thrown; a slow nudge, he tries to keep his feet. Once up he runs from it.
        // STAND-IN: no damage from the hit (IV's peds lose health by impact speed).
        if n.rag.is_none() && n.getup.is_none() && std::env::var("LIBERTY_OLD_CARHIT").is_err() {
            if let Some(def) = n.rag_def.clone() {
                let hips = t.translation + Vec3::Y * 0.9;
                let tuning = Tuning::default();
                let coming = solids
                    .iter()
                    .filter(|c| c.vel.length() > 1.0 && c.pos.distance(hips) < 20.0)
                    .filter_map(|c| crate::ped::time_to_hit(c, hips, 0.35).map(|at| (c, at)))
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                let walk = Vec3::new(-n.yaw.sin(), 0.0, -n.yaw.cos()) * n.speed;
                let start = match coming {
                    Some((c, at)) if c.vel.length() > 4.0 && at > 0.0 && at <= tuning.brace_prediction => Some((Mode::Brace { car: c.id }, walk, c.pos)),
                    Some((c, at)) if at <= 0.0 && c.vel.length() > 4.0 => Some((Mode::Relax, c.vel * 0.85 + Vec3::Y * 2.5, c.pos)),
                    Some((c, at)) if at <= 0.0 => Some((Mode::Balance, walk, c.pos)),
                    _ => None,
                };
                if let Some((mode, vel, from)) = start {
                    let locals: Vec<Transform> = if n.last_pose.len() == n.joints.len() {
                        n.last_pose.iter().map(|(r, p)| Transform { translation: *p, rotation: *r, ..default() }).collect()
                    } else {
                        n.joints.iter().map(|j| j.1).collect()
                    };
                    let globals = def.globals(&rig_of(&t), &locals);
                    info!("ped {entity}: car coming, {mode:?}");
                    n.rag = Some(RagdollSim::start(&def, &globals, vel, chunks.clone(), mode, tuning));
                    n.speed = 0.0;
                    n.fade = None;
                    n.panic = 9.0 + pool.rand() * 6.0;
                    n.flee_from = from;
                }
            }
        }
        if let (Some(def), Some(mut sim)) = (n.rag_def.clone(), n.rag.take()) {
            // lying still: leave him as he fell
            if sim.still < 2.0 {
                sim.step(&def, dt, chunks.clone(), &solids);
                let locals: Vec<Transform> = if n.last_pose.len() == n.joints.len() {
                    n.last_pose.iter().map(|(r, p)| Transform { translation: *p, rotation: *r, ..default() }).collect()
                } else {
                    n.joints.iter().map(|j| j.1).collect()
                };
                let pel = sim.pelvis_transform().translation;
                let g = collision::ground_height(chunks.clone(), pel.x, pel.z, pel.y + 0.5).unwrap_or(pel.y - 0.2);
                // in deep water: he drowns (IV's peds can't swim; its in-water task, not ported,
                // drowns them after a few seconds). STAND-IN: at once, and the body stays where
                // the ragdoll takes it (no floating). Fountains and pools are water too: only water
                // over 1 m deep counts. LIBERTY_OLD_WATER: before this (he gets up on the bottom).
                if n.health > 0.0 && nav.deep_water(pel.x, -pel.z, g).is_some_and(|w| pel.y < w - 0.3) {
                    n.health = 0.0;
                    info!("ped {entity}: drowned at {:.0}, {:.0}", pel.x, -pel.z);
                }
                t.translation = Vec3::new(pel.x, g, pel.z);
                let rig = rig_of(&t);
                let globals = sim.joint_globals(&def, &rig, &locals);
                let mut pose = crate::ragdoll::locals_from_globals(&def, &rig, &globals);
                with_helpers(&n, &mut pose);
                for (j, (rot, pos)) in pose.iter().enumerate() {
                    if let Ok(mut bt) = bones.get_mut(n.joints[j].0) {
                        bt.rotation = *rot;
                        bt.translation = *pos;
                    }
                }
                // the balancer held him up: back to walking (running away) from where he stands
                let stood = matches!(sim.mode, Mode::Balance) && pel.y - g > 0.6 && sim.still > 0.8;
                if sim.recovered || stood {
                    n.fade = Some((pose, 0.3));
                    n.speed = 0.0;
                    continue;
                }
                // come to rest and still alive: line up the game's get-up clip with how he lies
                // (face down or on his back, head-to-hips direction), as for Niko
                if sim.still > 1.0 && n.health > 0.0 {
                    let (pb, hb) = (def.pelvis_bone(), def.head_bone());
                    let belly = globals[pb].rotation * def.rest_global[pb].rotation.inverse() * Vec3::NEG_Z;
                    let clip_name = if belly.y < 0.0 { "getup_front_normal" } else { "getup_back_normal" };
                    let loco = n.loco.take();
                    let lined_up = loco.as_ref().and_then(|l| l.extra.get(clip_name)).map(|clip| {
                        let rest: Vec<Transform> = n.joints.iter().map(|j| j.1).collect();
                        let first: Vec<Transform> = clip.pose_at(0.0, &rest).into_iter().map(|(r, v)| Transform { translation: v, rotation: r, ..default() }).collect();
                        let cg = def.globals(&Transform::IDENTITY, &first);
                        let angle = |d: Vec3| d.x.atan2(d.z);
                        let d_clip = (cg[hb].translation - cg[pb].translation).with_y(0.0);
                        let d_world = (globals[hb].translation - globals[pb].translation).with_y(0.0);
                        let yaw = angle(d_world) - angle(d_clip);
                        let at = globals[pb].translation - Quat::from_rotation_y(yaw) * cg[pb].translation.with_y(0.0);
                        (Vec3::new(at.x, g, at.z), yaw)
                    });
                    n.loco = loco;
                    if let Some((at, yaw)) = lined_up {
                        n.getup = Some((clip_name.to_string(), 0.0, at, yaw));
                        n.yaw = yaw;
                        continue;
                    }
                }
            }
            n.rag = Some(sim);
            continue;
        }
        // getting up: the clip's own root motion carries him (and his height: these clips keep the
        // pelvis at the root and rise from 0 to standing)
        if let Some((clip_name, at_t, start, start_yaw)) = n.getup.take() {
            let loco = n.loco.take();
            let done = loco.as_ref().and_then(|l| l.extra.get(&clip_name)).map(|clip| {
                let now = at_t + dt;
                let (off, rot) = clip.mover_at(now);
                let p = start + Quat::from_rotation_y(start_yaw) * off;
                let g = collision::ground_height(chunks.clone(), p.x, p.z, start.y + 1.0).unwrap_or(start.y);
                t.translation = Vec3::new(p.x, g, p.z);
                n.yaw = start_yaw + rot.to_euler(EulerRot::YXZ).0;
                t.rotation = Quat::from_rotation_y(n.yaw);
                let rest: Vec<Transform> = n.joints.iter().map(|j| j.1).collect();
                let mut pose = clip.pose_at(now, &rest);
                if let Some(root) = pose.first_mut() {
                    root.1.y += off.y - HIP_HEIGHT;
                }
                with_helpers(&n, &mut pose);
                for (j, (r, v)) in pose.iter().enumerate() {
                    if let Ok(mut bt) = bones.get_mut(n.joints[j].0) {
                        bt.rotation = *r;
                        bt.translation = *v;
                    }
                }
                n.last_pose = pose;
                (now, now >= clip.duration)
            });
            n.loco = loco;
            match done {
                Some((now, false)) => {
                    n.getup = Some((clip_name, now, start, start_yaw));
                    continue;
                }
                Some((_, true)) => {
                    n.speed = 0.0;
                    n.timer = 0.0;
                    continue;
                }
                None => {}
            }
        }
        n.timer -= dt;
        if n.timer <= 0.0 {
            n.walking = pool.rand() < 0.8 || n.cross.is_some();
            n.timer = if n.walking { 3.0 + pool.rand() * 8.0 } else { 1.5 + pool.rand() * 4.0 };
            if n.walking && n.at.is_none() {
                n.yaw += (pool.rand() - 0.5) * 1.2;
            }
        }
        let walk_speed = n.loco.as_ref().map_or(1.5, |l| l.clips[1].speed);
        let mut target = if n.walking { walk_speed } else { 0.0 };
        // frightened: run (his own run clip's speed) directly away, until it wears off
        if n.panic > 0.0 {
            // running: off the route; he finds the pavement again afterwards
            n.at = None;
            n.route.clear();
            n.goal = None;
            n.rejoin = None;
            n.cross = None;
            n.look = 0.5;
            n.panic -= dt;
            let away = (t.translation - n.flee_from).with_y(0.0);
            if let Some((_, left)) = n.detour.as_mut() {
                *left -= dt;
            }
            if n.detour.is_some_and(|d| d.1 <= 0.0) {
                // still a drop straight away from the danger: keep running along it
                let ahead = t.translation + away.normalize_or_zero() * 1.5;
                if step_to(chunks.clone(), &nav, t.translation, ahead).is_none() {
                    n.detour = n.detour.map(|d| (d.0, 0.5));
                } else {
                    n.detour = None;
                }
            }
            if away.length_squared() > 0.01 && n.timer < 1.0e5 {
                let want = n.detour.map_or((-away.x).atan2(-away.z), |d| d.0);
                let d = (want - n.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                n.yaw += d * (dt * 9.0).min(1.0);
                // turn away first, then run: full speed only once he's facing away
                let facing_away = (1.0 - d.abs() / 1.2).clamp(0.0, 1.0);
                target = walk_speed * 0.4 + (n.loco.as_ref().map_or(4.5, |l| l.clips[2].speed.max(walk_speed)) - walk_speed * 0.4) * facing_away;
            } else {
                target = n.loco.as_ref().map_or(4.5, |l| l.clips[2].speed.max(walk_speed));
            }
        }
        // a road crossing: look for one ahead now and then, then walk to it, wait, cross
        let mut cross_aim = None;
        if n.panic <= 0.0 && n.timer < 1.0e5 {
            if let Some(roads) = roads.as_deref() {
                let fwd = Vec3::new(-n.yaw.sin(), 0.0, -n.yaw.cos());
                let mut rand = || pool.rand();
                if let Some((aim, pace)) = cross_road(roads, &solids, &mut n, t.translation, fwd, dt, time.elapsed_secs(), &mut rand) {
                    cross_aim = Some(aim);
                    let run = n.loco.as_ref().map_or(4.5, |l| l.clips[2].speed.max(walk_speed));
                    // IV's move blend: 1 walk, 2 run
                    target = if pace <= 0.0 { 0.0 } else { walk_speed + (run - walk_speed) * (pace - 1.0).clamp(0.0, 1.0) };
                }
            }
        }
        n.speed += (target - n.speed) * (dt * 3.0).min(1.0);

        // on the pavement: head along the way planned (straight lines through it); crossing:
        // head for the crossing's ends
        if n.panic <= 0.0 && n.timer < 1.0e5 {
            let mut rand = || pool.rand();
            let fwd = Vec3::new(-n.yaw.sin(), 0.0, -n.yaw.cos());
            let aim = match cross_aim {
                Some(a) => Some(a),
                None => follow_pavement(&mut nav, &mut n, t.translation, fwd, dt, &mut rand),
            };
            if let Some(aim) = aim {
                let to = (aim - t.translation).with_y(0.0);
                if to.length_squared() > 0.01 {
                    let want = (-to.x).atan2(-to.z);
                    let d = (want - n.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                    n.yaw += d * (dt * 5.0).min(1.0);
                }
            }
        }
        let fwd = Vec3::new(-n.yaw.sin(), 0.0, -n.yaw.cos());
        let feet = t.translation;
        let mut next = feet + fwd * n.speed * dt;
        let pushed = collision::push_out_of_walls(chunks.clone(), next, 0.3, &[0.5, 1.2]);
        if pushed.distance(next) > 0.01 && n.at.is_none() {
            n.yaw += std::f32::consts::PI * (0.6 + pool.rand() * 0.8); // bumped: turn away
        }
        next = pushed;
        if let Some(g) = step_to(chunks.clone(), &nav, feet, next) {
            next.y = g;
        } else {
            let stuck = next.distance(feet) > 1e-4;
            next = feet; // ledge, drop or water: don't walk off it
            if stuck && n.panic > 0.0 && n.detour.is_none() && !nav.old_water {
                // running away and the way is a drop: run along it, to whichever side is open
                // (STAND-IN: IV's fleeing peds route over the navmesh)
                let side = |a: f32| step_to(chunks.clone(), &nav, feet, feet + Vec3::new(-a.sin(), 0.0, -a.cos()) * 1.0).is_some();
                let (l, r) = (n.yaw + std::f32::consts::FRAC_PI_2, n.yaw - std::f32::consts::FRAC_PI_2);
                let pick = match (side(l), side(r)) {
                    (true, false) => l,
                    (false, true) => r,
                    (true, true) => if pool.rand() < 0.5 { l } else { r },
                    _ => n.yaw + std::f32::consts::PI,
                };
                n.detour = Some((pick, 2.0 + pool.rand() * 1.5));
            } else if n.at.is_none() && (n.panic <= 0.0 || nav.old_water) {
                n.yaw += std::f32::consts::PI;
            } else if stuck && n.panic <= 0.0 && n.cross.is_none() {
                // a drop across the way planned: plan again, the other way
                n.route.clear();
                n.goal = None;
                n.wander += std::f32::consts::PI;
            }
        }
        t.translation = next;
        t.rotation = t.rotation.slerp(Quat::from_rotation_y(n.yaw), (dt * 5.0).min(1.0));

        if let Some(loco) = n.loco.as_ref() {
            let cycle = pedanim::cycle_duration(loco, n.speed);
            let idle_len = loco.clips[0].duration;
            let phase = (n.phase + dt / cycle).rem_euclid(1.0);
            let idle_phase = (n.idle_phase + dt / idle_len).rem_euclid(1.0);
            let rest: Vec<Transform> = n.joints.iter().map(|j| j.1).collect();
            let mut pose = pedanim::pose(loco, n.speed, phase, idle_phase, &rest);
            if let Some((from, left)) = n.fade.take() {
                let w = (left / 0.3).clamp(0.0, 1.0);
                for (p, f) in pose.iter_mut().zip(&from) {
                    p.0 = p.0.slerp(f.0, w);
                    p.1 = p.1.lerp(f.1, w);
                }
                if left - dt > 0.0 {
                    n.fade = Some((from, left - dt));
                }
            }
            with_helpers(&n, &mut pose);
            for (j, (rot, pos)) in pose.iter().enumerate() {
                if let Ok(mut bt) = bones.get_mut(n.joints[j].0) {
                    bt.rotation = *rot;
                    bt.translation = *pos;
                }
            }
            n.last_pose = pose;
            n.phase = phase;
            n.idle_phase = idle_phase;
        }
    }
}

/// The ground a ped standing at `feet` can step to at `at`: not a ledge or drop, and not into water
/// (IV's peds keep out of it; one that ends up in deep water drowns, in `move_peds`).
fn step_to<'a>(chunks: impl IntoIterator<Item = &'a collision::ChunkCollision> + Clone, nav: &Nav, feet: Vec3, at: Vec3) -> Option<f32> {
    collision::ground_height(chunks, at.x, at.z, feet.y + 0.6)
        .filter(|g| (g - feet.y).abs() < 0.6 && (nav.old_water || nav.water_level(at.x, -at.z).is_none_or(|w| *g > w + 0.2)))
}

/// The arm skin helper bones follow the arms: IV moves them in code (no clip drives them), with the
/// rules measured for Niko (`ped::place_helpers`); peds share the skeleton layout. Without this the
/// forearm and upper-arm skin stays at the rest pose while the arms move (chasm's "mutant" fat ped).
/// `LIBERTY_NO_PED_HELPERS` (or `LIBERTY_NO_HELPERS`, Niko too) turns it off (before/after runs).
fn with_helpers(n: &Npc, pose: &mut [(Quat, Vec3)]) {
    if std::env::var("LIBERTY_NO_HELPERS").is_err() && std::env::var("LIBERTY_NO_PED_HELPERS").is_err() && pose.len() == n.joints.len() {
        let rest: Vec<Transform> = n.joints.iter().map(|j| j.1).collect();
        place_helpers(&n.helpers, &rest, pose);
    }
}

/// The navmesh tiles, read from `navmeshes.img` as they're needed.
#[derive(Resource)]
pub struct Nav {
    img: Option<Img>,
    tiles: HashMap<u16, Option<Arc<rage::navmesh::NavMesh>>>,
    /// water.dat's quads (GTA space), for keeping peds out of the water.
    water: Vec<rage::water::WaterQuad>,
    old_water: bool,
}

impl Nav {
    pub fn open(root: &Path, key: &Key) -> Nav {
        let img = Img::open(&root.join("pc/data/cdimages/navmeshes.img"), key).ok();
        if img.is_none() {
            warn!("navmeshes.img not opened: peds wander instead of walking the pavements");
        }
        let mut water = std::fs::read_to_string(root.join("common/data/water.dat")).ok().and_then(|t| rage::water::read_water(&t).ok()).unwrap_or_default();
        water.extend(crate::testpad::lab_water()); // the test lab's pool
        Nav { img, tiles: HashMap::new(), water, old_water: std::env::var("LIBERTY_OLD_WATER").is_ok() }
    }

    /// The water surface height at a map point (GTA x, y), if there's water over 1 m deep above
    /// `ground` (Bevy height = GTA z) there. None with LIBERTY_OLD_WATER (before/after runs).
    pub fn deep_water(&self, x: f32, y: f32, ground: f32) -> Option<f32> {
        if self.old_water {
            return None;
        }
        self.water_level(x, y).filter(|w| *w > ground + 1.0)
    }

    /// The water surface height at a map point (GTA x, y), if there's water there.
    pub fn water_level(&self, x: f32, y: f32) -> Option<f32> {
        self.water
            .iter()
            .filter(|q| {
                let (a, b) = (q.corners[0], q.corners[3]);
                x >= a[0].min(b[0]) && x <= a[0].max(b[0]) && y >= a[1].min(b[1]) && y <= a[1].max(b[1])
            })
            .map(|q| q.corners.iter().map(|c| c[2]).fold(f32::MIN, f32::max))
            .reduce(f32::max)
    }

    fn tile(&mut self, area: u16) -> Option<Arc<rage::navmesh::NavMesh>> {
        if !self.tiles.contains_key(&area) {
            let tile = self.img.as_ref().and_then(|img| {
                let e = img.find(&rage::navmesh::tile_name(area))?;
                rage::navmesh::read(&Rsc::load(&img.read(e).ok()?).ok()?).ok().map(Arc::new)
            });
            self.tiles.insert(area, tile);
        }
        self.tiles.get(&area)?.clone()
    }

    /// What the navmesh says is under a position: "pavement", "other" (road, grass, ...) or
    /// "nothing" (no polygon there). For tests.
    pub fn under(&mut self, pos: Vec3) -> &'static str {
        let (x, y) = (pos.x, -pos.z);
        let Some(tile) = rage::navmesh::area_at(x, y).and_then(|a| self.tile(a)) else { return "nothing" };
        let mut found = "nothing";
        for p in &tile.polys {
            let n = p.corners.len();
            // point in polygon (x, y), and near his height (bridges and roofs are other layers)
            let inside = (0..n).fold(false, |inside, i| {
                let (a, b) = (p.corners[i], p.corners[(i + 1) % n]);
                if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] { !inside } else { inside }
            });
            if inside && (p.centre()[2] - pos.y).abs() < 2.0 {
                if p.pavement() {
                    return "pavement";
                }
                found = "other";
            }
        }
        found
    }

    fn poly(&mut self, id: (u16, u16)) -> Option<(Arc<rage::navmesh::NavMesh>, usize)> {
        let tile = self.tile(id.0)?;
        ((id.1 as usize) < tile.polys.len()).then_some((tile, id.1 as usize))
    }
}

/// One edge of the way planned: its two ends (Bevy space), the way across it, and the polygon
/// beyond.
#[derive(Clone, Copy)]
struct Portal {
    l: Vec3,
    r: Vec3,
    n: Vec3,
    into: (u16, u16),
}

impl Portal {
    /// The edge pulled in from its ends (he keeps clear of the kerb and the walls).
    fn inner(&self) -> (Vec3, Vec3) {
        let len = self.l.distance(self.r);
        let k = if len > 1e-3 { 0.45f32.min(len * 0.3) / len } else { 0.0 };
        (self.l.lerp(self.r, k), self.l.lerp(self.r, 1.0 - k))
    }
}

/// Do segments a-b and c-d cross, on the ground plane?
fn crosses(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> bool {
    let cr = |o: Vec3, p: Vec3, q: Vec3| (p.x - o.x) * (q.z - o.z) - (p.z - o.z) * (q.x - o.x);
    let (d1, d2, d3, d4) = (cr(c, d, a), cr(c, d, b), cr(a, b, c), cr(a, b, d));
    d1 * d2 <= 0.0 && d3 * d4 <= 0.0
}

/// The pavement polygon under a position, if any.
fn pavement_at(nav: &mut Nav, pos: Vec3) -> Option<(u16, u16)> {
    let (x, y) = (pos.x, -pos.z);
    let tile = rage::navmesh::area_at(x, y).and_then(|a| nav.tile(a))?;
    tile.polys.iter().enumerate().find_map(|(i, p)| {
        let n = p.corners.len();
        let inside = n >= 3
            && (0..n).fold(false, |inside, k| {
                let (a, b) = (p.corners[k], p.corners[(k + 1) % n]);
                if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] { !inside } else { inside }
            });
        (inside && p.pavement() && (p.centre()[2] - pos.y).abs() < 2.0).then_some((tile.area, i as u16))
    })
}

/// IV's wandering ped (CTaskComplexMoveWander): he keeps a heading, and once a second it moves
/// halfway to the way he's actually facing (so it bends with the pavement); he asks for a route
/// of 8-20 m in that heading (the path server's wander search, pavement only), and asks for the
/// next one when he's within 4 m of its end. Ours: from the polygon he's in, take the
/// neighbouring pavement polygon whose edge lies most nearly along the heading, until the route
/// is long enough (a dead end turns him round), and walk it in straight lines: toward the
/// furthest point on it he can see through every edge between (edges pulled in 0.45 m from their
/// ends), so he keeps a line along a sidewalk and turns at its corners. Off the pavement he walks
/// back to the nearest pavement polygon within 25 m. STAND-IN: the route search itself (IV's runs
/// in the path server thread, not read).
fn follow_pavement(nav: &mut Nav, n: &mut Npc, pos: Vec3, fwd: Vec3, dt: f32, rand: &mut dyn FnMut() -> f32) -> Option<Vec3> {
    // the heading follows his facing, half way each second (exe: 0.5 each, every 1000 ms)
    n.wander_at -= dt;
    if n.wander_at <= 0.0 {
        n.wander_at = 1.0;
        let h = Vec3::new(-n.wander.sin(), 0.0, -n.wander.cos()) * 0.5 + fwd * 0.5;
        if h.length_squared() > 1e-4 {
            n.wander = (-h.x).atan2(-h.z);
        }
    }
    if n.at.is_none() {
        if let Some((poly, c)) = n.rejoin {
            if c.with_y(pos.y).distance(pos) > 1.0 && pavement_at(nav, pos) != Some(poly) {
                return Some(c);
            }
            n.rejoin = None;
            n.at = Some(poly);
        } else {
            n.look -= dt;
            if n.look > 0.0 {
                return None;
            }
            n.look = 1.0;
            if let Some(here) = pavement_at(nav, pos) {
                n.at = Some(here);
            } else {
                let tile = rage::navmesh::area_at(pos.x, -pos.z).and_then(|a| nav.tile(a))?;
                let near = tile
                    .polys
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.pavement() && p.corners.len() >= 3 && (p.centre()[2] - pos.y).abs() < 3.0)
                    .map(|(i, p)| (Vec3::from(to_bevy(p.centre())).distance(pos), i))
                    .filter(|(d, _)| *d < 25.0)
                    .min_by(|a, b| a.0.total_cmp(&b.0))?;
                let c = Vec3::from(to_bevy(tile.polys[near.1].centre()));
                n.rejoin = Some(((tile.area, near.1 as u16), c));
                return Some(c);
            }
        }
        n.came = None;
        n.route.clear();
        n.goal = None;
    }
    // passed the next edge: he's in the polygon it led to
    while let Some(p) = n.route.front() {
        if (pos - (p.l + p.r) * 0.5).with_y(0.0).dot(p.n) <= 0.0 {
            break;
        }
        n.came = n.at;
        n.at = Some(p.into);
        n.route.pop_front();
    }
    // pushed well off the way (a crowd, a car): find the pavement again
    if n.route.front().is_some_and(|p| p.l.lerp(p.r, 0.5).with_y(pos.y).distance(pos) > 15.0) {
        n.at = None;
        n.look = 0.0;
        return None;
    }
    // near the end of the way planned: plan on from its end
    if n.goal.is_none_or(|g| g.with_y(pos.y).distance(pos) < 4.0) {
        plan_route(nav, n, pos, rand);
    }
    let goal = n.goal?;
    // the furthest point along the way he can walk to in a straight line
    let route = &n.route;
    let clear = |to: Vec3, upto: usize| {
        route.iter().take(upto).all(|p| {
            let (a, b) = p.inner();
            crosses(pos, to, a, b)
        })
    };
    if clear(goal, route.len()) {
        return Some(goal);
    }
    for i in (0..route.len()).rev() {
        let (a, b) = route[i].inner();
        // where the line toward the goal meets this edge, kept within it
        let (d, e) = (goal - pos, b - a);
        let den = d.x * e.z - d.z * e.x;
        let s = if den.abs() > 1e-6 { ((a.x - pos.x) * d.z - (a.z - pos.z) * d.x) / den } else { 0.5 };
        let at = a.lerp(b, s.clamp(0.0, 1.0));
        if i == 0 || clear(at, i) {
            return Some(at);
        }
    }
    Some(goal)
}

/// Add 8-20 m of pavement to a ped's way, from where it ends now, along his heading.
fn plan_route(nav: &mut Nav, n: &mut Npc, pos: Vec3, rand: &mut dyn FnMut() -> f32) {
    let length = 8.0 + 12.0 * rand(); // exe: 8 .. 20 m
    let heading = Vec3::new(-n.wander.sin(), 0.0, -n.wander.cos());
    let (mut last, mut before) = match n.route.len() {
        0 => match n.at {
            Some(a) => (a, n.came),
            None => return,
        },
        1 => (n.route[0].into, n.at),
        k => (n.route[k - 1].into, Some(n.route[k - 2].into)),
    };
    let mut from = n.route.back().map_or(pos, |p| (p.l + p.r) * 0.5);
    let mut seen: Vec<(u16, u16)> = n.route.iter().map(|p| p.into).chain(n.at).collect();
    let mut went = 0.0;
    while went < length && n.route.len() < 24 {
        let Some((tile, i)) = nav.poly(last) else { break };
        let poly = &tile.polys[i];
        let centre = Vec3::from(to_bevy(poly.centre()));
        let count = poly.corners.len();
        let mut best: Option<(f32, Portal)> = None;
        let mut back: Option<Portal> = None;
        for e in 0..count {
            let next = poly.across[e];
            if next == last {
                continue;
            }
            let Some((nt, ni)) = nav.poly(next) else { continue };
            if !nt.polys[ni].pavement() || nt.polys[ni].corners.len() < 3 {
                continue;
            }
            let (a, b) = (Vec3::from(to_bevy(poly.corners[e])), Vec3::from(to_bevy(poly.corners[(e + 1) % count])));
            if a.distance(b) < 0.6 {
                continue; // too narrow to walk through
            }
            let mid = (a + b) * 0.5;
            let mut nrm = Vec3::new(b.z - a.z, 0.0, a.x - b.x).normalize_or_zero();
            if nrm.dot(mid - centre) < 0.0 {
                nrm = -nrm;
            }
            let portal = Portal { l: a, r: b, n: nrm, into: next };
            if Some(next) == before || seen.contains(&next) {
                if Some(next) == before {
                    back = Some(portal);
                }
                continue;
            }
            let score = (mid - from).with_y(0.0).normalize_or_zero().dot(heading) * 0.5 + nrm.dot(heading) * 0.5 + 0.3 * rand();
            if best.as_ref().is_none_or(|x| score > x.0) {
                best = Some((score, portal));
            }
        }
        let step = match (best, back) {
            // the pavement goes on, not straight back the way he came
            (Some((score, p)), _) if score > -0.35 || n.route.is_empty() => p,
            // a dead end: turn round (only as the first step; otherwise the way ends here)
            (_, Some(p)) if n.route.is_empty() => {
                n.wander += std::f32::consts::PI;
                p
            }
            _ => break,
        };
        let mid = (step.l + step.r) * 0.5;
        went += mid.distance(from);
        from = mid;
        before = Some(last);
        last = step.into;
        seen.push(last);
        n.route.push_back(step);
    }
    // the way ends just across its last edge
    n.goal = n.route.back().map(|p| {
        let (a, b) = p.inner();
        a.lerp(b, 0.25 + 0.5 * rand()) + p.n * 0.5
    });
}

/// A road crossing in progress: its two ends (with IV's jitter), the light's phase over the
/// road (0: no light), what the signal showed when he decided, and where he's got to.
pub struct Crossing {
    start: Vec3,
    end: Vec3,
    phase: u8,
    first_signal: u8,
    changed: bool,
    /// 0 going to the kerb, 1 waiting there, 2 crossing.
    stage: u8,
    /// Seconds to the next look at the light (waiting) or the next "hurry?" (crossing).
    wait: f32,
    /// How long he's been waiting.
    waited: f32,
    /// IV's move blend: 1 walk .. 2 run.
    pace: f32,
}

/// IV's road crossing (exe: the wander task 0xcb1750 every 2 s calls 0xcbb1c0, then
/// CTaskComplexMoveCrossRoadAtTrafficLights):
/// - Look: the nearest crossing node within 12 m, if it's at most 8 m away, within 3 m of his
///   height and within 45 degrees of where he's facing; of the nodes across the road from it,
///   the one most ahead of him. Start and end points jittered up to 1 m along and across the
///   crossing (the node's width byte, 0 everywhere, + 1). Not again for 8-16 s after crossing.
/// - Walk to the start at move blend 1.0-1.5; with a light (IV: a light within 10 m of the
///   node), stand 1-2 s at a time until the signal shows walk AND has changed since he decided
///   (so he never starts late in a walk phase); without one, wait for a gap in the traffic
///   (STAND-IN for IV's own wait task 0xcac380: no car moving within 25 m that's coming at the
///   crossing).
/// - Cross at move blend 1.0-1.5; if the signal goes off with more than 4 m to go, a 15% chance
///   each second to hurry to 1.75-2.25.
///
/// Returns where he heads and his move blend (0 = stand) while a crossing is on.
#[allow(clippy::too_many_arguments)]
fn cross_road(roads: &Roads, cars: &[CarBox], n: &mut Npc, pos: Vec3, fwd: Vec3, dt: f32, time: f32, rand: &mut dyn FnMut() -> f32) -> Option<(Vec3, f32)> {
    n.cross_cool -= dt;
    if n.cross.is_none() {
        n.cross_check -= dt;
        if n.cross_check > 0.0 || n.cross_cool > 0.0 || !n.walking || n.speed < 0.3 {
            return None;
        }
        n.cross_check = 2.0;
        let (ai, a) = roads
            .cross_pos
            .iter()
            .enumerate()
            .map(|(i, p)| (i, *p))
            .min_by(|x, y| x.1.distance_squared(pos).total_cmp(&y.1.distance_squared(pos)))?;
        let to = a - pos;
        if to.with_y(0.0).length() > 8.0 || to.y.abs() > 3.0 || to.with_y(0.0).normalize_or_zero().dot(fwd) < 0.707 {
            return None;
        }
        let &(bi, phase) = roads.cross_links[ai].iter().max_by(|x, y| {
            let ahead = |i: u32| (roads.cross_pos[i as usize] - pos).with_y(0.0).normalize_or_zero().dot(fwd);
            ahead(x.0).total_cmp(&ahead(y.0))
        })?;
        let b = roads.cross_pos[bi as usize];
        let d = (b - a).with_y(0.0).normalize_or_zero();
        let side = Vec3::new(-d.z, 0.0, d.x);
        let mut jit = || 2.0 * rand() - 1.0;
        let start = a + d * jit() + side * jit();
        let end = b + d * jit() + side * jit();
        let first = traffic::walk_signal(d.x, -d.z, time);
        n.cross = Some(Crossing { start, end, phase, first_signal: first, changed: false, stage: 0, wait: 0.0, waited: 0.0, pace: 1.0 + 0.5 * rand() });
        n.route.clear();
        n.goal = None;
    }
    let c = n.cross.as_mut()?;
    let way = c.end - c.start;
    let signal = if c.phase == 0 { 0 } else { traffic::walk_signal(way.x, -way.z, time) };
    c.changed |= signal != c.first_signal;
    match c.stage {
        0 => {
            if c.start.with_y(pos.y).distance(pos) < 1.0 {
                c.stage = 1;
                c.wait = 0.0;
            }
            Some((c.start, c.pace))
        }
        1 => {
            c.waited += dt;
            c.wait -= dt;
            if c.wait <= 0.0 {
                c.wait = 1.0 + rand(); // exe: stand 1000 + 0..1023 ms, then look again
                let go = if c.phase != 0 {
                    signal == 0 && c.changed
                } else {
                    let mid = (c.start + c.end) * 0.5;
                    !cars.iter().any(|car| {
                        let off = (mid - car.pos).with_y(0.0);
                        car.vel.length() > 1.0 && off.length() < 25.0 && car.vel.dot(off) > 0.0
                    })
                };
                if go {
                    c.stage = 2;
                    c.wait = 1.0;
                    c.pace = 1.0 + 0.5 * rand();
                } else if c.waited > 90.0 {
                    // given up: back the way he came
                    n.wander += std::f32::consts::PI;
                    n.cross = None;
                    n.cross_cool = 8.0;
                    return None;
                }
            }
            // stand facing across
            Some((pos + (c.end - c.start).with_y(0.0).normalize_or_zero() * 0.3, 0.0))
        }
        _ => {
            let left = c.end.with_y(pos.y).distance(pos);
            c.wait -= dt;
            if c.wait <= 0.0 {
                c.wait = 1.0;
                if c.phase != 0 && signal != 0 && left > 4.0 && c.pace < 1.75 && rand() < 5000.0 / 32768.0 {
                    c.pace = 1.75 + 0.5 * rand();
                }
            }
            if left < 1.0 {
                // across: back to the pavement, heading on the way he crossed
                let d = (c.end - c.start).with_y(0.0);
                n.wander = (-d.x).atan2(-d.z);
                n.cross = None;
                n.cross_cool = 8.0 + 8.191 * rand(); // exe: 8000 + 0..8191 ms
                n.at = None;
                n.rejoin = None;
                n.look = 0.0;
                return None;
            }
            Some((c.end, c.pace))
        }
    }
}
