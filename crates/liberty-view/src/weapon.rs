//! Weapons in Niko's hand: the game's own models (`weapons.img`) and numbers (`WeaponInfo.xml`).
//! Keys: 1-9 pick a gun, 0 holsters, X cycles through everything, right mouse aims, left mouse
//! fires (while aiming). Clip size, time between shots, reload time, range, damage and impact
//! force are the game's own numbers.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;
use rage::drawable::{self, Drawable};
use rage::wtd::{self, Format, Texture};
use rage::weapons::Weapon;
use rage::{Img, Key, Resource as Rsc};

use crate::assets;
use crate::collision;
use crate::ped::{Player, PlayerRes};
use crate::peds::Npc;
use crate::stream::City;
use crate::vehicle::Car;

pub struct WeaponDef {
    pub info: Weapon,
    model: Option<(Drawable, HashMap<String, Texture>)>,
    /// the model's own `gun_grip` bone (Bevy axes, model space): the point and frame that sit on
    /// the hand bone
    grip: Option<Transform>,
    /// the model's `gun_muzzle` bone (Bevy axes, model space): where shots leave
    muzzle: Option<Vec3>,
}

#[derive(Resource)]
pub struct Weapons(pub Vec<WeaponDef>);

impl WeaponDef {
    /// the weapon's HUD picture (the `icon` texture in its pack)
    pub fn icon(&self) -> Option<&Texture> {
        self.model.as_ref()?.1.get("icon")
    }

    /// the model's transform in the hand bone's frame
    pub fn hold(&self) -> Transform {
        hold_transform(self.grip)
    }
}

/// What Niko is holding right now.
#[derive(Resource, Default)]
pub struct Armed {
    /// index into `Weapons`, None = fists
    pub weapon: Option<usize>,
    pub aiming: bool,
    /// rounds left in the clip, per weapon (filled to the clip size the first time it's picked)
    clip: HashMap<usize, u32>,
    /// seconds until the next shot may leave / until the reload finishes (of `reload_total`)
    cooldown: f32,
    reload: f32,
    reload_total: f32,
    /// R was pressed: reload as soon as it makes sense
    want_reload: bool,
    /// 1 at a shot, fading to 0: the recoil kick on the aim pose
    pub kick: f32,
    /// shots fired so far (the sound system plays one fire sound per count)
    pub shots: u32,
}

/// A bullet hit something: what (an entity, or None for the world), where, which way it was
/// travelling, and the weapon's damage and impact force.
#[derive(Event)]
pub struct BulletHit {
    pub entity: Option<Entity>,
    pub point: Vec3,
    pub dir: Vec3,
    pub damage: f32,
    pub force: f32,
}

/// A short-lived effect (tracer, flash, spark, bullet mark): seconds left.
#[derive(Component)]
pub struct Fx(f32);

/// The model entity of weapon `.0`, child of the right-hand bone.
#[derive(Component)]
pub struct HeldModel(usize);

/// 1-9 pick these, in order
const KEY_ORDER: &[&str] = &["PISTOL", "DEAGLE", "SHOTGUN", "BARETTA", "MICRO_UZI", "MP5", "AK47", "M4", "SNIPERRIFLE"];

pub fn load(root: &Path, key: &Key) -> Result<Weapons> {
    let list = rage::weapons::load(root)?;
    let img = Img::open(&root.join("pc/models/cdimages/weapons.img"), key)?;
    let mut out = Vec::new();
    for info in list {
        let model = (!info.model.is_empty()).then(|| -> Result<_> {
            let e = img.find(&format!("{}.wdr", info.model)).with_context(|| format!("{}.wdr", info.model))?;
            let res = Rsc::load(&img.read(e)?)?;
            let d = drawable::read(&res)?;
            // weapon models carry their own skeleton: gun_grip (right hand), gun_grip2 (left
            // hand), gun_muzzle, gun_ejac; bone positions are relative to the root bone, which
            // sits where the mesh's origin is
            let skeleton = drawable::read_skeleton(&res, Rsc::ROOT)?;
            let muzzle = skeleton.iter().find(|b| b.name == "gun_muzzle").map(|b| Vec3::from(assets::to_bevy(b.local_pos)));
            let grip = skeleton.into_iter().find(|b| b.name == "gun_grip").map(|b| {
                let [qx, qy, qz, qw] = b.local_rot;
                Transform {
                    translation: Vec3::from(assets::to_bevy(b.local_pos)),
                    rotation: Quat::from_xyzw(qx, qz, -qy, qw).normalize(),
                    ..default()
                }
            });
            let mut textures = HashMap::new();
            if let Some(e) = img.find(&format!("{}.wtd", info.model)) {
                for t in wtd::read(&Rsc::load(&img.read(e)?)?)? {
                    textures.insert(t.name.to_ascii_lowercase(), t);
                }
            }
            Ok((d, textures, grip, muzzle))
        });
        let (model, grip, muzzle) = match model {
            Some(Ok((d, t, g, m))) => (Some((d, t)), g, m),
            Some(Err(e)) => {
                warn!("weapon {}: model {} not loaded: {e:#}", info.name, info.model);
                (None, None, None)
            }
            None => (None, None, None),
        };
        out.push(WeaponDef { info, model, grip, muzzle });
    }
    info!("weapons: {} ({} with models)", out.len(), out.iter().filter(|w| w.model.is_some()).count());
    Ok(Weapons(out))
}

fn vec3_env(name: &str, default: [f32; 3]) -> Vec3 {
    let v: Vec<f32> = std::env::var(name).unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
    if v.len() == 3 { Vec3::new(v[0], v[1], v[2]) } else { Vec3::from(default) }
}

/// Where the weapon sits in the hand bone's frame: its `gun_grip` bone on the hand bone (the grip
/// bone sits behind the gun, at the wrist, like the hand bone's origin). Debug nudges on top, in the
/// hand's GTA axes (x along the fingers): LIBERTY_GUN_POS (m) / LIBERTY_GUN_ROT (deg).
fn hold_transform(grip: Option<Transform>) -> Transform {
    let base = grip.map_or(Transform::from_xyz(0.06, 0.0, 0.0), |g| Transform::from_matrix(g.compute_matrix().inverse()));
    let pos = vec3_env("LIBERTY_GUN_POS", [0.0, 0.0, 0.0]);
    let rot = vec3_env("LIBERTY_GUN_ROT", [0.0, 0.0, 0.0]);
    // the hand's GTA-space frame → Bevy (x, z, -y), same change of basis as the bones
    let to_b = Quat::from_mat3(&Mat3::from_cols(Vec3::X, Vec3::NEG_Z, Vec3::Y));
    let r = Quat::from_euler(EulerRot::XYZ, rot.x.to_radians(), rot.y.to_radians(), rot.z.to_radians());
    let nudge = Transform { translation: Vec3::new(pos.x, pos.z, -pos.y), rotation: to_b * r * to_b.inverse(), ..default() };
    nudge * base
}

/// Put every weapon model in Niko's right hand (hidden until picked).
pub fn spawn_held(
    mut commands: Commands,
    weapons: Res<Weapons>,
    data: Res<PlayerRes>,
    player: Query<&Player>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Ok(p) = player.single() else { return };
    let Some(hand) = data.0.bone_index("char_r_hand") else { return };
    let hand_e = p.joint_entity(hand);
    info!("weapons: {} models into bone {hand} (char_r_hand)", weapons.0.iter().filter(|w| w.model.is_some()).count());
    for (i, w) in weapons.0.iter().enumerate() {
        let Some((d, textures)) = &w.model else { continue };
        let hold = w.hold();
        if w.grip.is_none() {
            warn!("weapon {}: no gun_grip bone, held at a default spot", w.info.name);
        }
        let holder = commands.spawn((hold, Visibility::Hidden, HeldModel(i), Name::new(format!("held {}", w.info.name)))).id();
        commands.entity(hand_e).add_child(holder);
        let mut cache: HashMap<String, Handle<Image>> = HashMap::new();
        let mats: Vec<Handle<StandardMaterial>> = d
            .shaders
            .iter()
            .map(|s| {
                let t = s.diffuse.as_ref().and_then(|n| textures.get(n));
                let tex = t.map(|t| cache.entry(t.name.clone()).or_insert_with(|| images.add(assets::gpu_image(t))).clone());
                let masked = t.is_some_and(|t| matches!(t.format, Format::Dxt3 | Format::Dxt5)) && s.name.contains("alpha");
                let mut m = assets::material(tex, if masked { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque });
                m.perceptual_roughness = 0.5;
                let normal = s.normal.as_ref().and_then(|n| textures.get(n)).map(|t| cache.entry(format!("{}#linear", t.name)).or_insert_with(|| images.add(assets::gpu_image_linear(t))).clone());
                let specular = s.specular.as_ref().and_then(|n| textures.get(n)).map(|t| cache.entry(t.name.clone()).or_insert_with(|| images.add(assets::gpu_image(t))).clone());
                assets::apply_maps(&mut m, normal, specular);
                materials.add(m)
            })
            .collect();
        for m in &d.meshes {
            let mat = mats.get(m.shader).cloned().unwrap_or_else(|| materials.add(assets::material(None, AlphaMode::Opaque)));
            let e = commands.spawn((Mesh3d(meshes.add(assets::build_mesh(m))), MeshMaterial3d(mat))).id();
            commands.entity(holder).add_child(e);
        }
    }
}

/// 1-9 / 0 / X pick the weapon; LIBERTY_WEAPON=<name> starts with one.
pub fn select(keys: Res<ButtonInput<KeyCode>>, buttons: Res<ButtonInput<MouseButton>>, weapons: Res<Weapons>, mut armed: ResMut<Armed>, mut first: Local<bool>) {
    let find = |n: &str| weapons.0.iter().position(|w| w.info.name.eq_ignore_ascii_case(n) && w.model.is_some());
    if !*first {
        *first = true;
        if let Ok(n) = std::env::var("LIBERTY_WEAPON") {
            armed.weapon = find(&n);
        }
    }
    let digits = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9];
    for (k, name) in digits.iter().zip(KEY_ORDER) {
        if keys.just_pressed(*k) {
            armed.weapon = find(name);
        }
    }
    if keys.just_pressed(KeyCode::Digit0) {
        armed.weapon = None;
    }
    // R reloads (on foot; in a car R is "back on its wheels")
    if keys.just_pressed(KeyCode::KeyR) && armed.weapon.is_some() {
        armed.want_reload = true;
    }
    if keys.just_pressed(KeyCode::KeyX) {
        let n = weapons.0.len();
        let start = armed.weapon.map_or(0, |i| i + 1);
        armed.weapon = (start..n).find(|&i| weapons.0[i].model.is_some());
    }
    // right mouse aims (IV on PC); Z too, for testing without a mouse
    let want = buttons.pressed(MouseButton::Right) || keys.pressed(KeyCode::KeyZ) || std::env::var("LIBERTY_AIM").is_ok();
    let aiming = armed.weapon.is_some_and(|i| weapons.0[i].info.is_gun()) && want;
    if armed.aiming != aiming {
        armed.aiming = aiming;
    }
}

/// Show the picked weapon's model, hide the rest.
pub fn show_held(armed: Res<Armed>, mut models: Query<(&HeldModel, &mut Visibility)>) {
    let _slow = crate::SlowGuard::new("weapon::show_held");
    if !armed.is_changed() {
        return;
    }
    for (m, mut v) in &mut models {
        *v = if armed.weapon == Some(m.0) { Visibility::Inherited } else { Visibility::Hidden };
    }
}

/// LIBERTY_AIM_DEBUG: log where the drawn gun's barrel points, next to the camera's direction.
pub fn debug_barrel(armed: Res<Armed>, models: Query<(&HeldModel, &GlobalTransform)>, cam: Query<&GlobalTransform, With<Camera3d>>, player: Query<&GlobalTransform, With<Player>>, mut n: Local<u32>) {
    if std::env::var("LIBERTY_AIM_DEBUG").is_err() {
        return;
    }
    *n += 1;
    if *n % 30 != 0 {
        return;
    }
    let yaw = |v: Vec3| (-v.x).atan2(-v.z).to_degrees();
    let cam_f = cam.single().map(|c| c.forward().as_vec3()).unwrap_or_default();
    let body_f = player.single().map(|c| c.forward().as_vec3()).unwrap_or_default();
    for (m, g) in &models {
        if armed.weapon == Some(m.0) {
            let barrel = g.rotation() * Vec3::X;
            info!("aim debug: camera yaw {:.0}, body facing yaw {:.0}, barrel yaw {:.0} pitch {:.0}", yaw(cam_f), yaw(body_f), yaw(barrel), barrel.y.asin().to_degrees());
        }
    }
}

impl Armed {
    /// Reloading: (seconds into it, how long it takes).
    pub fn reload_progress(&self) -> Option<(f32, f32)> {
        (self.reload > 0.0).then(|| (self.reload_total - self.reload, self.reload_total))
    }
}

/// Rounds left in the picked weapon's clip, and whether it's reloading (for the HUD).
pub fn ammo(armed: &Armed, weapons: &Weapons) -> Option<(String, u32, u32, bool)> {
    let i = armed.weapon?;
    let w = &weapons.0[i].info;
    w.is_gun().then(|| (w.name.clone(), armed.clip.get(&i).copied().unwrap_or(w.clip_size), w.clip_size, armed.reload > 0.0))
}

/// Left mouse while aiming: a shot along the camera's line of sight (hit-scan, like IV's
/// INSTANT_HIT weapons), at the weapon's own rate of fire, until the clip is empty; then its own
/// reload time. For tests LIBERTY_FIRE=1 holds the trigger, LIBERTY_FIRE_AT=<secs> holds it from
/// that many seconds into a recording, for LIBERTY_FIRE_FOR=<secs>.
#[allow(clippy::too_many_arguments)]
pub fn fire(
    mut commands: Commands,
    time: Res<Time>,
    buttons: Res<ButtonInput<MouseButton>>,
    weapons: Res<Weapons>,
    mut armed: ResMut<Armed>,
    city: Res<City>,
    cam: Query<&GlobalTransform, With<Camera3d>>,
    player: Query<&GlobalTransform, With<Player>>,
    models: Query<(&HeldModel, &GlobalTransform)>,
    cars: Query<(Entity, &Car)>,
    npcs: Query<(Entity, &GlobalTransform, &Npc)>,
    mut hits: EventWriter<BulletHit>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut assets: Local<Option<FxAssets>>,
    started: Option<Res<crate::RecordStart>>,
) {
    let _slow = crate::SlowGuard::new("weapon::fire");
    let dt = time.delta_secs().min(0.05);
    armed.cooldown = (armed.cooldown - dt).max(0.0);
    armed.kick = (armed.kick - dt / 0.12).max(0.0);
    let Some(i) = armed.weapon else { return };
    let w = &weapons.0[i];
    if !w.info.is_gun() || w.info.fire_type != "INSTANT_HIT" {
        return;
    }
    let size = w.info.clip_size.max(1);
    let left = *armed.clip.entry(i).or_insert(size);
    if armed.reload > 0.0 {
        armed.reload -= dt;
        if armed.reload <= 0.0 {
            armed.clip.insert(i, size);
        }
        return;
    }
    if left == 0 || (armed.want_reload && left < size) {
        armed.reload = w.info.reload_ms / 1000.0;
        armed.reload_total = armed.reload;
        armed.want_reload = false;
        return;
    }
    armed.want_reload = false;
    // LIBERTY_FIRE_FOR=<secs> lets go again after that long (default: keeps firing)
    let hold: f32 = std::env::var("LIBERTY_FIRE_FOR").ok().and_then(|v| v.parse().ok()).unwrap_or(f32::MAX);
    let scripted = std::env::var("LIBERTY_FIRE_AT").ok().and_then(|v| v.parse::<f32>().ok()).zip(started.as_ref()).is_some_and(|(at, s)| {
        let t = time.elapsed_secs() - s.0;
        t >= at && t < at + hold
    });
    let trigger = buttons.pressed(MouseButton::Left) || std::env::var("LIBERTY_FIRE").is_ok() || scripted;
    if !(armed.aiming && trigger && armed.cooldown <= 0.0) {
        return;
    }
    let (Ok(cam), Ok(me)) = (cam.single(), player.single()) else { return };
    armed.clip.insert(i, left - 1);
    armed.cooldown = w.info.time_between_shots_ms / 1000.0;
    armed.kick = 1.0;
    armed.shots += 1;

    // the shot: from the camera through the middle of the screen, starting past Niko
    let dir = cam.forward().as_vec3();
    let eye = cam.translation();
    let skip = ((me.translation() + Vec3::Y * 1.4 - eye).dot(dir) + 0.4).max(0.0);
    let origin = eye + dir * skip;
    let range = w.info.weapon_range.max(1.0);
    let mut best: Option<(f32, Vec3, Option<Entity>)> = collision::raycast(city.collision.values(), origin, dir, range).map(|(t, n)| (t, n, None));
    for (e, car) in &cars {
        let b = car.solid(e.to_bits());
        if let Some((t, n)) = ray_box(b.rot.inverse() * (origin - b.pos), b.rot.inverse() * dir, b.lo, b.hi).filter(|h| h.0 <= range) {
            if best.is_none_or(|x| t < x.0) {
                best = Some((t, b.rot * n, Some(e)));
            }
        }
    }
    for (e, g, npc) in &npcs {
        // a standing person: a 0.3 m thick column from the shins to the head; on the ground, a
        // low blob where he lies
        let feet = g.translation();
        let (a, b, r) = if npc.is_down() { (feet + Vec3::Y * 0.15, feet + Vec3::Y * 0.3, 0.45) } else { (feet + Vec3::Y * 0.25, feet + Vec3::Y * 1.65, 0.3) };
        if let Some(t) = ray_capsule(origin, dir, a, b, r).filter(|t| *t <= range) {
            if best.is_none_or(|x| t < x.0) {
                best = Some((t, -dir, Some(e)));
            }
        }
    }
    let end = origin + dir * best.map_or(range, |b| b.0);
    if let Some((_, _, entity)) = best {
        hits.write(BulletHit { entity, point: end, dir, damage: w.info.damage, force: w.info.force });
    }
    if std::env::var("LIBERTY_AIM_DEBUG").is_ok() {
        info!("shot: {} left {}/{size}, hit {:?} at {:.1} m", w.info.name, left - 1, best.map(|b| b.2), best.map_or(range, |b| b.0));
    }

    // --- what you see: flash at the muzzle, a tracer, a spark and a mark where it lands
    let fx = assets.get_or_insert_with(|| FxAssets::new(&mut meshes, &mut materials));
    let muzzle = models
        .iter()
        .find(|(m, _)| m.0 == i)
        .map(|(_, g)| g.transform_point(w.muzzle.unwrap_or_default()))
        .unwrap_or(me.translation() + Vec3::Y * 1.4);
    commands.spawn((PointLight { intensity: crate::lights::lumens(12.0, 12.0), range: 12.0, color: Color::srgb(1.0, 0.8, 0.5), shadows_enabled: false, ..default() }, Transform::from_translation(muzzle + dir * 0.1), Fx(0.05)));
    commands.spawn((Mesh3d(fx.ball.clone()), MeshMaterial3d(fx.flash.clone()), Transform::from_translation(muzzle + dir * 0.06).with_scale(Vec3::splat(0.07)), NotShadowCaster, Fx(0.04)));
    let len = (end - muzzle).length();
    if len > 0.5 {
        commands.spawn((
            Mesh3d(fx.cube.clone()),
            MeshMaterial3d(fx.tracer.clone()),
            Transform::from_translation((muzzle + end) * 0.5).looking_at(end, Vec3::Y).with_scale(Vec3::new(0.012, 0.012, len)),
            NotShadowCaster,
            Fx(0.05),
        ));
    }
    if let Some((_, n, entity)) = best {
        commands.spawn((Mesh3d(fx.ball.clone()), MeshMaterial3d(fx.flash.clone()), Transform::from_translation(end + n * 0.02).with_scale(Vec3::splat(0.06)), NotShadowCaster, Fx(0.07)));
        if entity.is_none() {
            // a bullet mark on the world, flat on the surface
            commands.spawn((
                Mesh3d(fx.ball.clone()),
                MeshMaterial3d(fx.mark.clone()),
                Transform::from_translation(end + n * 0.004).with_rotation(Quat::from_rotation_arc(Vec3::Y, n)).with_scale(Vec3::new(0.035, 0.004, 0.035)),
                NotShadowCaster,
                Fx(30.0),
            ));
        }
    }
}

/// Shared meshes and materials for the shot effects.
pub struct FxAssets {
    cube: Handle<Mesh>,
    ball: Handle<Mesh>,
    tracer: Handle<StandardMaterial>,
    flash: Handle<StandardMaterial>,
    mark: Handle<StandardMaterial>,
}

impl FxAssets {
    fn new(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) -> FxAssets {
        let glow = |c: Color, e: f32| StandardMaterial { base_color: c, emissive: c.to_linear() * e, unlit: true, ..default() };
        FxAssets {
            cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
            ball: meshes.add(Sphere::new(0.5)),
            tracer: materials.add(glow(Color::srgb(1.0, 0.9, 0.6), 4.0)),
            flash: materials.add(glow(Color::srgb(1.0, 0.85, 0.5), 8.0)),
            mark: materials.add(StandardMaterial { base_color: Color::srgb(0.03, 0.03, 0.03), perceptual_roughness: 1.0, ..default() }),
        }
    }
}

/// Effects fade out.
pub fn fx_decay(mut commands: Commands, time: Res<Time>, mut fx: Query<(Entity, &mut Fx)>) {
    for (e, mut f) in &mut fx {
        f.0 -= time.delta_secs();
        if f.0 <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

/// Ray against an axis-aligned box (the ray already in the box's frame): distance and face normal.
fn ray_box(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<(f32, Vec3)> {
    let (mut t0, mut t1, mut n) = (0.0f32, f32::MAX, Vec3::ZERO);
    for k in 0..3 {
        if d[k].abs() < 1e-8 {
            if o[k] < lo[k] || o[k] > hi[k] {
                return None;
            }
            continue;
        }
        let (mut a, mut b) = ((lo[k] - o[k]) / d[k], (hi[k] - o[k]) / d[k]);
        let mut face = Vec3::ZERO;
        face[k] = -d[k].signum();
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        if a > t0 {
            t0 = a;
            n = face;
        }
        t1 = t1.min(b);
        if t0 > t1 {
            return None;
        }
    }
    (t0 > 0.0).then_some((t0, n))
}

/// Ray against a capsule (segment a-b, radius r): distance along the ray to the closest approach,
/// if it passes within r.
fn ray_capsule(o: Vec3, d: Vec3, a: Vec3, b: Vec3, r: f32) -> Option<f32> {
    let (u, w) = (b - a, o - a);
    let (uu, ud, dd) = (u.dot(u), u.dot(d), d.dot(d));
    let (uw, dw) = (u.dot(w), d.dot(w));
    let den = uu * dd - ud * ud;
    // closest points between the ray (t) and the segment (s)
    let mut s = if den.abs() > 1e-8 { (uw * dd - dw * ud) / den } else { 0.0 }.clamp(0.0, 1.0);
    let mut t = (s * ud - dw) / dd;
    if t < 0.0 {
        t = 0.0;
        s = (uw / uu).clamp(0.0, 1.0);
    }
    ((o + d * t) - (a + u * s)).length_squared().le(&(r * r)).then_some(t)
}
