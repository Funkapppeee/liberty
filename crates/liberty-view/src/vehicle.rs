//! Cars: load any road car from `vehicles.img`, drop it on the street, drive it.
//! Rigid-body physics in `carphys.rs`, tuned by the game's handling.dat; paint from carcols.dat.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use bevy::prelude::*;
use bevy::render::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::render::mesh::VertexAttributeValues;
use rage::drawable::{self, Bone};
use rage::wtd::{self, Format, Texture};
use rage::{Img, Key, Resource as Rsc};

use crate::assets::{self, to_bevy};
use crate::carphys::{self, Body, Input, Params, Wheel};
use crate::collision;
use crate::stream::City;
use crate::Fly;

/// Physics sub-steps per second.
const HZ: f32 = 120.0;
/// At most this many physics steps per frame, so a slow frame can't snowball.
const MAX_STEPS: u32 = 6;

#[derive(Resource)]
pub struct Garage {
    img: Img,
    shared: HashMap<String, Texture>,
    pub models: Vec<String>,
    pub current: usize,
    handling: HashMap<String, rage::vehicles::Handling>,
    pub paint: HashMap<String, Color>,
    /// `visualSettings.dat`: the game's own numbers for car lights.
    pub visual: rage::visual::Visual,
}

/// A car's lights. `glass` = its lamp-glass materials (shader `gta_vehicle_lightsemissive`).
#[derive(Component)]
pub struct CarLights {
    glass: Vec<Handle<StandardMaterial>>,
    on: bool,
    glow: f32,
}

/// One of a car's real lights (a child of the car).
#[derive(Component)]
pub struct CarLamp;

impl Garage {
    pub fn open(root: &Path, key: &Key) -> Result<Garage> {
        let img = Img::open(&root.join("pc/models/cdimages/vehicles.img"), key)?;
        let mut models: Vec<String> = img
            .entries
            .iter()
            .filter(|e| e.ext() == "wft")
            .map(|e| e.name.trim_end_matches(".wft").to_ascii_lowercase())
            .collect();
        models.sort();
        // Shared car textures (tyres, glass, generic trim) live in vehshare(_truck).wtd inside
        // vehicles.img; the loose pc/textures/vehicle.wtd is something else.
        let mut shared: HashMap<String, Texture> = HashMap::new();
        for pack in ["vehshare.wtd", "vehshare_truck.wtd"] {
            if let Some(e) = img.find(pack) {
                for t in wtd::read(&Rsc::load(&img.read(e)?)?)? {
                    shared.entry(t.name.to_ascii_lowercase()).or_insert(t);
                }
            }
        }
        // Game data: which models are road cars, how each one drives, and its factory paint.
        let data = root.join("common/data");
        let defs = rage::vehicles::read_vehicles_ide(&std::fs::read_to_string(data.join("vehicles.ide"))?)?;
        let all_handling = rage::vehicles::read_handling(&std::fs::read_to_string(data.join("handling.dat"))?)?;
        let cols = rage::vehicles::read_carcols(&std::fs::read_to_string(data.join("carcols.dat"))?)?;
        let cars: std::collections::HashSet<String> =
            defs.iter().filter(|d| d.kind.eq_ignore_ascii_case("car")).map(|d| d.model.to_ascii_lowercase()).collect();
        models.retain(|m| cars.contains(m));
        let mut handling = HashMap::new();
        let mut paint = HashMap::new();
        for d in &defs {
            let model = d.model.to_ascii_lowercase();
            if let Some(h) = all_handling.get(&d.handling_id.to_ascii_lowercase()) {
                handling.insert(model.clone(), h.clone());
            }
            let first = cols.combos.get(&model).and_then(|c| c.first()).and_then(|c| c.first());
            if let Some(rgb) = first.and_then(|&i| cols.palette.get(i as usize)) {
                paint.insert(model, Color::linear_rgb(rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0));
            }
        }
        let want = std::env::var("LIBERTY_CAR").unwrap_or_else(|_| "infernus".into()).to_ascii_lowercase();
        let current = models.iter().position(|m| *m == want).unwrap_or(0);
        info!("garage: {} road cars, {} with handling, {} with paint", models.len(), handling.len(), paint.len());
        let visual = rage::visual::Visual::load(root)?;
        Ok(Garage { img, shared, models, current, handling, paint, visual })
    }

    pub fn current_name(&self) -> &str {
        &self.models[self.current]
    }
}

fn bone_local(b: &Bone) -> Transform {
    let [qx, qy, qz, qw] = b.local_rot;
    Transform {
        translation: Vec3::from(to_bevy(b.local_pos)),
        rotation: Quat::from_xyzw(qx, qz, -qy, qw).normalize(),
        ..default()
    }
}

#[derive(Component)]
pub struct Car {
    /// Seconds since spawn (the scripted test throttle counts from here).
    age: f32,
    body: Body,
    params: Params,
    wheels: Vec<Wheel>,
    hull: Vec<Vec3>,
    steer: f32,
    /// The steering stick, eased (carphys::steer_angle).
    stick: f32,
    half_width: f32,
    /// Leftover time for the fixed-rate physics.
    accumulator: f32,
    /// The body transform last physics step, for the chase camera's smoothing.
    pub speed_kmh: f32,
    /// Chase camera heading: swings toward the direction of travel with a lag (GTA IV style).
    cam_yaw: Option<f32>,
    /// Body pose before the last physics step, so frames can be drawn between steps (no judder).
    prev: (Vec3, Quat),
    /// Body box, local (for car-to-car contacts).
    bounds: (Vec3, Vec3),
    /// Skeleton joints (entity, rest local transform, rest global rotation), by bone index.
    joints: Vec<(Entity, Transform, Quat)>,
    /// Rest local position of `seat_dside_f`, in the car's local Bevy space.
    driver_seat: Option<Vec3>,
    /// Driver's front door joint, if this model has one: entity, rest local transform, rest
    /// global (car-local) rotation.
    door: Option<(Entity, Transform, Quat)>,
    /// 0 = shut, 1 = fully open.
    pub door_open: f32,
    /// For the engine sound: the model's name (its audio object in game.dat16), gears from
    /// handling.dat, top speed (m/s), and the throttle and reverse inputs of the last frame.
    pub model: String,
    pub gears: u32,
    pub top_speed: f32,
    pub throttle: f32,
    pub reversing: bool,
}

#[derive(Component)]
pub struct WheelVis {
    idx: usize,
}

/// Marker on a car's skeleton joint entities, so `animate_doors`'s queries don't collide
/// with `drive`'s (which touches the car transform and `WheelVis`).
#[derive(Component)]
pub struct CarJoint;

impl Car {
    /// The `seat_dside_f` bone's rest position, in the car's local Bevy space.
    pub fn driver_seat(&self) -> Option<Vec3> {
        self.driver_seat
    }

    /// Move a car that something else is driving (traffic): keeps the physics body in step so
    /// the player can take it over mid-drive.
    pub fn set_pose(&mut self, pos: Vec3, rot: Quat, vel: Vec3) {
        self.body.pos = pos;
        self.body.rot = rot;
        self.body.vel = vel;
        self.body.ang = Vec3::ZERO;
        self.prev = (pos, rot);
        self.accumulator = 0.0;
    }

    /// Distance from the body origin down to the ground when the car sits at rest.
    pub fn ride_height(&self) -> f32 {
        // the suspension mount is chosen so wheels settle exactly at their model positions
        let w = self.wheels.iter().map(|w| w.rest.y).fold(0.0f32, f32::min);
        -w + self.wheels.first().map_or(0.35, |w| w.radius)
    }

    /// The car as a moving box, for the ragdoll's physics world.
    pub fn solid(&self, id: u64) -> crate::ragdoll::CarBox {
        crate::ragdoll::CarBox { id, pos: self.body.pos, rot: self.body.rot, vel: self.body.vel, lo: self.bounds.0, hi: self.bounds.1 }
    }

    /// The physics body's position and rotation.
    pub fn body_pose(&self) -> (Vec3, Quat) {
        (self.body.pos, self.body.rot)
    }
}

/// Load the garage's current model and spawn it with its wheels on the ground at `ground`.
pub fn spawn_car(
    commands: &mut Commands,
    garage: &Garage,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    ground: Vec3,
    yaw: f32,
) -> Result<Entity> {
    let name = garage.current_name().to_string();
    spawn_car_named(commands, garage, meshes, materials, images, bindposes, ground, yaw, &name)
}

/// Spawn a given road car model (traffic picks its own; the player's comes from the garage).
#[allow(clippy::too_many_arguments)]
pub fn spawn_car_named(
    commands: &mut Commands,
    garage: &Garage,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    ground: Vec3,
    yaw: f32,
    name: &str,
) -> Result<Entity> {
    let name = name.to_string();
    let entry = garage.img.find(&format!("{name}.wft")).with_context(|| format!("{name}.wft"))?;
    let v = drawable::read_vehicle(&Rsc::load(&garage.img.read(entry)?)?)?;
    let own: HashMap<String, Texture> = match garage.img.find(&format!("{name}.wtd")) {
        Some(e) => wtd::read(&Rsc::load(&garage.img.read(e)?)?)?.into_iter().map(|t| (t.name.to_ascii_lowercase(), t)).collect(),
        None => HashMap::new(),
    };
    let paint = garage.paint.get(&name).copied().unwrap_or(Color::linear_rgb(0.08, 0.18, 0.42));

    let mut image_cache: HashMap<String, Handle<Image>> = HashMap::new();
    let mut glass: Vec<Handle<StandardMaterial>> = Vec::new();
    let mats: Vec<Handle<StandardMaterial>> = v
        .body
        .shaders
        .iter()
        .map(|s| {
            let tex = s.diffuse.as_ref().and_then(|n| own.get(n).or_else(|| garage.shared.get(n)).map(|t| (n, t)));
            if let (Some(n), None) = (&s.diffuse, &tex) {
                warn!("{name}: texture {n} not found (shader {})", s.name);
            }
            let handle = tex.map(|(n, t)| image_cache.entry(n.clone()).or_insert_with(|| images.add(assets::gpu_image(t))).clone());
            let masked = tex.is_some_and(|(_, t)| matches!(t.format, Format::Dxt3 | Format::Dxt5));
            let mut m = assets::material(handle, if masked { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque });
            let find = |n: &Option<String>| n.as_ref().and_then(|n| own.get(n).or_else(|| garage.shared.get(n)));
            let normal = find(&s.normal).map(|t| image_cache.entry(format!("{}#linear", t.name)).or_insert_with(|| images.add(assets::gpu_image_linear(t))).clone());
            let specular = find(&s.specular).map(|t| image_cache.entry(t.name.clone()).or_insert_with(|| images.add(assets::gpu_image(t))).clone());
            assets::apply_maps(&mut m, normal, specular);
            if s.name.contains("paint") {
                m.base_color = paint;
                m.perceptual_roughness = 0.25;
                m.metallic = 0.35;
                m.reflectance = 0.6;
            } else if s.name.contains("glass") {
                m.alpha_mode = AlphaMode::Blend;
                m.base_color = Color::srgba(0.6, 0.65, 0.7, 0.55);
                m.perceptual_roughness = 0.05;
            }
            if s.name.contains("lightsemissive") {
                // lamp glass: lit by its own texture, strength set by `car_lights`
                m.emissive_texture = m.base_color_texture.clone();
                m.emissive = LinearRgba::BLACK;
                let h = materials.add(m);
                glass.push(h.clone());
                return h;
            }
            materials.add(m)
        })
        .collect();
    let fallback = materials.add(assets::material(None, AlphaMode::Opaque));
    let mat = |i: usize| mats.get(i).cloned().unwrap_or_else(|| fallback.clone());

    // Wheels from the rig; radius from the wheel mesh.
    let wheel_meshes = v.wheel.clone().unwrap_or_default();
    let radius = wheel_meshes
        .iter()
        .flat_map(|m| m.positions.iter())
        .map(|p| p[2].abs().max(p[1].abs()))
        .fold(0.0f32, f32::max)
        .max(0.25);
    let wheels: Vec<Wheel> = v
        .wheels()
        .map(|b| Wheel {
            rest: Vec3::from(to_bevy(b.abs_pos)),
            front: b.name.ends_with('f'),
            right: b.name.contains("_r"),
            radius,
            compression: 0.0,
            contact: false,
            spin: 0.0,
            slip: 0.0,
        })
        .collect();
    let (lo, hi) = (Vec3::from(to_bevy(v.body.bounds_min)), Vec3::from(to_bevy(v.body.bounds_max)));
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    let params = match garage.handling.get(&name) {
        Some(h) => Params::from_handling(h, lo, hi),
        None => Params::fallback(lo, hi),
    };
    let wheel_y = wheels.iter().map(|w| w.rest.y).fold(0.0f32, f32::min);
    let hull = carphys::hull_points(lo, hi, wheel_y);
    // set it down settled: wheels at their modelled positions on the ground, where IV's springs
    // carry exactly their share of the weight (dropped in fully extended, the soft, lightly damped
    // springs kept it bobbing for seconds: the ram test's car met Niko mid-bounce)
    let ride = -wheel_y + radius;
    let rot = Quat::from_rotation_y(yaw);
    let body = Body { pos: ground + Vec3::Y * (ride + 0.01), rot, vel: Vec3::ZERO, ang: Vec3::ZERO, gearbox: Default::default() };

    let wheel_parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>)> =
        wheel_meshes.iter().map(|m| (meshes.add(assets::build_mesh(m)), mat(m.shader))).collect();

    info!(
        "spawned {name}: {} wheels, r {radius:.2} m, {:.0} kg, drive {:.2}g bias {:.2}, top {:.0} km/h, susp {:.2} [{:.2}..{:.2}]",
        wheels.len(),
        params.mass,
        params.drive_force,
        params.drive_bias_front,
        params.top_speed * 3.6,
        params.susp_force,
        params.susp_lower,
        params.susp_upper
    );
    let wheel_rest: Vec<Vec3> = wheels.iter().map(|w| w.rest).collect();

    let car_e = commands.spawn((Transform::from_translation(body.pos).with_rotation(rot), Visibility::default())).id();

    // Skeleton joints (doors, bonnet, boot, seats...), parents before children as stored.
    let mut joints: Vec<(Entity, Transform, Quat)> = Vec::with_capacity(v.bones.len());
    let mut global: Vec<Mat4> = Vec::with_capacity(v.bones.len());
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    for (i, b) in v.bones.iter().enumerate() {
        let local = bone_local(b);
        let e = commands.spawn((local, CarJoint, Name::new(b.name.clone()))).id();
        let parent_g = match b.parent {
            Some(p) => {
                commands.entity(joints[p].0).add_child(e);
                global[p]
            }
            None => {
                commands.entity(car_e).add_child(e);
                Mat4::IDENTITY
            }
        };
        let g = parent_g * local.compute_matrix();
        let (_, g_rot, _) = g.to_scale_rotation_translation();
        global.push(g);
        joints.push((e, local, g_rot));
        by_name.insert(b.name.as_str(), i);
    }
    // Lights at the model's own light bones, with the game's numbers (visualSettings.dat):
    // two headlights tilted down by car.headlight.angle, two tail lights. Hidden until
    // `car_lights` turns them on. Cone angles are used as half-angles, like the map's lights.
    let vis = &garage.visual;
    let spot = |kind: &str, color: [f32; 3]| {
        let (range, outer) = (vis.get(&format!("car.{kind}.falloffMax"), 20.0), vis.get(&format!("car.{kind}.outerConeAngle"), 50.0).to_radians().min(1.5));
        let inner = vis.get(&format!("car.{kind}.innerConeAngle"), 20.0).to_radians().min(outer);
        let c = vis.rgb(&format!("car.{kind}.color"), color);
        SpotLight {
            color: Color::linear_rgb(c[0], c[1], c[2]),
            intensity: crate::lights::lumens(vis.get(&format!("car.{kind}.intensity"), 3.0), range),
            range,
            outer_angle: outer,
            inner_angle: inner,
            shadows_enabled: false,
            ..default()
        }
    };
    let tilt = vis.get("car.headlight.angle", -0.2);
    let mut lamps = 0;
    for (i, b) in v.bones.iter().enumerate() {
        let (kind, back) = if b.name.starts_with("headlight") {
            ("headlight", false)
        } else if b.name.starts_with("taillight") {
            ("taillight", true)
        } else {
            continue;
        };
        let at = global[i].to_scale_rotation_translation().2;
        let dir = if back { Vec3::Z } else { Vec3::new(0.0, tilt.sin(), -tilt.cos()) };
        let light = spot(kind, if back { [1.0, 0.0, 0.0] } else { [1.0, 1.0, 0.8] });
        let lamp = commands.spawn((light, Transform::from_translation(at).looking_to(dir, Vec3::Y), Visibility::Hidden, CarLamp)).id();
        commands.entity(car_e).add_child(lamp);
        lamps += 1;
    }
    if std::env::var("LIBERTY_LIGHT_DEBUG").is_ok() {
        info!("{name}: {lamps} lights at its light bones ({:?})", v.bones.iter().filter(|b| b.name.contains("light")).map(|b| b.name.as_str()).collect::<Vec<_>>());
    }
    commands.entity(car_e).insert(CarLights { glass, on: false, glow: -1.0 });

    let inverse = bindposes.add(SkinnedMeshInverseBindposes::from(global.iter().map(|m| m.inverse()).collect::<Vec<_>>()));
    let joint_entities: Vec<Entity> = joints.iter().map(|j| j.0).collect();
    let door = by_name.get("door_dside_f").map(|&i| joints[i]);
    let driver_seat = by_name.get("seat_dside_f").map(|&i| Vec3::from(to_bevy(v.bones[i].abs_pos)));

    // Body meshes: skinned (doors etc. can move) where the rig carries weights, else static.
    commands.entity(car_e).with_children(|c| {
        for m in &v.body.meshes {
            let mut mesh = assets::build_mesh(m);
            let skinned = m.weights.len() == m.positions.len();
            if skinned {
                mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(m.joints.clone()));
                mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, m.weights.clone());
            }
            let mut e = c.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(mat(m.shader))));
            if skinned {
                e.insert(SkinnedMesh { inverse_bindposes: inverse.clone(), joints: joint_entities.clone() });
            }
        }
        for (idx, &at) in wheel_rest.iter().enumerate() {
            c.spawn((Transform::from_translation(at), Visibility::default(), WheelVis { idx })).with_children(|w| {
                for (mesh, m) in &wheel_parts {
                    w.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(m.clone())));
                }
            });
        }
    });

    let prev0 = (body.pos, body.rot);
    let top_speed = params.top_speed;
    commands.entity(car_e).insert(Car {
        age: 0.0,
        body,
        params,
        wheels,
        hull,
        steer: 0.0,
        stick: 0.0,
        half_width: (hi.x - lo.x) / 2.0,
        accumulator: 0.0,
        speed_kmh: 0.0,
        cam_yaw: None,
        prev: prev0,
        bounds: (lo, hi),
        joints,
        driver_seat,
        door,
        door_open: 0.0,
        gears: garage.handling.get(&name).map_or(5, |h| h.gears.clamp(1, 8)),
        top_speed,
        model: name,
        throttle: 0.0,
        reversing: false,
    });
    Ok(car_e)
}

/// C: get in a car in front of you (or out of it); [ / ] change model; R: put back on its wheels.
#[allow(clippy::too_many_arguments)]
pub fn car_controls(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut garage: ResMut<Garage>,
    city: Res<City>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut cam: Query<(&mut Transform, &mut Fly), Without<Car>>,
    mut cars: Query<(Entity, &mut Transform, &mut Car), Without<crate::traffic::Traffic>>,
    roads: Option<Res<crate::traffic::Roads>>,
    player: Query<&Transform, (With<crate::ped::Player>, Without<Car>, Without<Fly>)>,
) {
    let Ok((mut cam_t, mut fly)) = cam.single_mut() else { return };
    let auto_spawn = std::env::var("LIBERTY_DRIVE").is_ok() && fly.driving.is_none() && !city.collision.is_empty();
    let cycle = if keys.just_pressed(KeyCode::BracketRight) {
        1
    } else if keys.just_pressed(KeyCode::BracketLeft) {
        -1
    } else {
        0
    };
    let mut respawn_at: Option<(Vec3, f32)> = None;
    // (spot, yaw, drive it straight away?)
    let mut drive_it = auto_spawn;
    // LIBERTY_PARK: park one car in front of you as soon as there's ground (tests)
    let auto_park = std::env::var("LIBERTY_PARK").is_ok() && cars.is_empty() && !city.collision.is_empty();

    if (keys.just_pressed(KeyCode::KeyC) && fly.driving.is_none()) || auto_spawn || auto_park {
        // C parks a car across your path, driver's door towards you (F gets in)
        let fwd = Vec3::new(cam_t.forward().x, 0.0, cam_t.forward().z).normalize_or(Vec3::NEG_Z);
        let mut spot = cam_t.translation + fwd * if auto_spawn { 6.0 } else { 8.0 };
        // the test car parks 5 m ahead of Niko (from the camera it sometimes ended up out of his reach)
        if auto_park {
            if let Ok(p) = player.single() {
                spot = p.translation + fwd * 5.0;
            }
        }
        // a car to drive goes on the nearest real lane, facing along it (not in a planter)
        let mut lane_dir = None;
        if auto_spawn {
            if let Some((q, d)) = roads.as_deref().and_then(|r| crate::traffic::nearest_lane(r, spot, fwd, 40.0)) {
                spot = q;
                lane_dir = Some(d);
            }
        }
        if let Some(g) = collision::ground_height(city.collision.values(), spot.x, spot.z, cam_t.translation.y + 2.0) {
            let yaw = if let Some(d) = lane_dir {
                (-d.x).atan2(-d.z)
            } else if auto_spawn {
                fwd.x.atan2(fwd.z) + std::f32::consts::PI
            } else {
                // car forward = (fwd.z, 0, -fwd.x) puts its left (driver's) side facing us
                let f = Vec3::new(fwd.z, 0.0, -fwd.x);
                (-f.x).atan2(-f.z)
            };
            respawn_at = Some((Vec3::new(spot.x, g, spot.z), yaw));
        } else if !auto_spawn && !auto_park {
            warn!("no ground in front of you to put a car on (is the chunk loaded?)");
        }
    }
    if cycle != 0 {
        let n = garage.models.len() as i32;
        garage.current = ((garage.current as i32 + cycle).rem_euclid(n)) as usize;
        if let Some(e) = fly.driving.take() {
            if let Ok((_, t, _)) = cars.get(e) {
                let yaw = t.rotation.to_euler(EulerRot::YXZ).0;
                let g = collision::ground_height(city.collision.values(), t.translation.x, t.translation.z, t.translation.y + 1.0);
                respawn_at = Some((Vec3::new(t.translation.x, g.unwrap_or(t.translation.y - 1.0), t.translation.z), yaw));
            }
            commands.entity(e).despawn();
            drive_it = true;
        }
    }
    if keys.just_pressed(KeyCode::KeyR) {
        if let Some(e) = fly.driving {
            if let Ok((_, _, mut car)) = cars.get_mut(e) {
                let yaw = car.body.rot.to_euler(EulerRot::YXZ).0;
                car.body.rot = Quat::from_rotation_y(yaw);
                car.body.pos.y += 1.5;
                car.body.vel = Vec3::ZERO;
                car.body.ang = Vec3::ZERO;
                car.prev = (car.body.pos, car.body.rot);
            }
        }
    }
    if let Some((pos, yaw)) = respawn_at {
        // one car at a time for now
        for (e, _, _) in &cars {
            commands.entity(e).despawn();
        }
        match spawn_car(&mut commands, &garage, &mut meshes, &mut materials, &mut images, &mut bindposes, pos, yaw) {
            Ok(e) => {
                if drive_it {
                    fly.driving = Some(e);
                    fly.walking = false;
                }
            }
            Err(err) => error!("could not load {}: {err:#}", garage.current_name()),
        }
    }
}

/// LIBERTY_CAR_TRACE=<file>: every physics step of the driven car as a text line (step count, GTA
/// position x y z, pitch, roll, yaw in degrees, forward speed m/s, wheel compressions), for
/// comparing its ride with IV's own cars (re/car_ride.py).
fn trace_step(car: &Car) {
    use std::io::Write;
    static TRACE: std::sync::OnceLock<Option<std::sync::Mutex<(std::fs::File, u64)>>> = std::sync::OnceLock::new();
    let Some(file) = TRACE.get_or_init(|| std::env::var("LIBERTY_CAR_TRACE").ok().and_then(|p| std::fs::File::create(p).ok()).map(|f| std::sync::Mutex::new((f, 0)))) else { return };
    let Ok(mut g) = file.lock() else { return };
    let (y, p, r) = car.body.rot.to_euler(EulerRot::YXZ);
    let pos = car.body.pos;
    let v = car.body.vel.dot(car.body.rot * Vec3::NEG_Z);
    let comps: Vec<String> = car.wheels.iter().map(|w| format!("{:.4}", w.compression)).collect();
    let n = g.1;
    let _ = writeln!(g.0, "{n} {:.4} {:.4} {:.4} {:.3} {:.3} {:.3} {:.3} {}", pos.x, -pos.z, pos.y, p.to_degrees(), r.to_degrees(), y.to_degrees(), v, comps.join(" "));
    g.1 += 1;
}

/// Run the car physics at a fixed rate, pose the wheels, and put the camera behind the car.
pub fn drive(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    city: Res<City>,
    mut cars: Query<(&mut Transform, &mut Car, &Children)>,
    mut wheel_vis: Query<(&mut Transform, &WheelVis), (Without<Car>, Without<Fly>)>,
    mut cam: Query<(&mut Transform, &Fly), (Without<Car>, Without<WheelVis>)>,
    started: Option<Res<crate::RecordStart>>,
) {
    let _slow = crate::SlowGuard::new("vehicle::drive");
    let Ok((mut cam_t, fly)) = cam.single_mut() else { return };
    let Some(car_e) = fly.driving else { return };
    let Ok((mut t, mut car, children)) = cars.get_mut(car_e) else { return };
    let dt = time.delta_secs().min(0.1);
    car.age += dt;

    // --- input (smoothed steering, less lock at speed)
    // LIBERTY_DRIVE=secs: full throttle for that long — counted from the recording's start when recording
    let recording = std::env::args().any(|a| a == "--record");
    let script_t = if recording { started.as_ref().map(|s| time.elapsed_secs() - s.0) } else { Some(car.age) };
    let scripted = std::env::var("LIBERTY_DRIVE").ok().and_then(|s| s.parse::<f32>().ok()).is_some_and(|s| script_t.is_some_and(|t| t < s))
        // LIBERTY_THROTTLE_AT=from,to (test lab): full throttle then, in a car he got into himself
        || std::env::var("LIBERTY_THROTTLE_AT").ok().and_then(|s| s.split_once(',').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))).is_some_and(|(a, b)| script_t.is_some_and(|t| t >= a && t < b));
    let steer_script = std::env::var("LIBERTY_DRIVE_STEER").ok().and_then(|s| s.parse::<f32>().ok()).filter(|_| scripted);
    let steer_in = steer_script
        .unwrap_or((keys.pressed(KeyCode::KeyA) as i32 - keys.pressed(KeyCode::KeyD) as i32) as f32);
    let speed = car.body.vel.length();
    (car.stick, car.steer) = carphys::steer_angle(&car.params, &car.body, car.stick, steer_in, dt);
    let input = Input {
        throttle: if keys.pressed(KeyCode::KeyW) || scripted { 1.0 } else { 0.0 },
        // LIBERTY_BRAKE_AT=secs (test lab): the brake held from then on (counted like LIBERTY_DRIVE), until stopped
        reverse: if keys.pressed(KeyCode::KeyS) || std::env::var("LIBERTY_BRAKE_AT").ok().and_then(|s| s.parse::<f32>().ok()).is_some_and(|b| script_t.is_some_and(|t| t >= b) && speed > 0.3) { 1.0 } else { 0.0 },
        handbrake: keys.pressed(KeyCode::Space),
        steer: car.steer,
    };
    car.throttle = input.throttle;
    car.reversing = input.reverse > 0.0;

    // --- fixed-rate physics
    car.accumulator += dt;
    let step = 1.0 / HZ;
    let car = &mut *car;
    car.accumulator = car.accumulator.min(step * MAX_STEPS as f32);
    while car.accumulator >= step {
        car.accumulator -= step;
        car.prev = (car.body.pos, car.body.rot);
        if !carphys::step(&mut car.body, &car.params, &mut car.wheels, &car.hull, input, city.collision.values(), step) {
            car.accumulator = 0.0; // no collision streamed in yet: hold still
            break;
        }
        trace_step(car);
    }
    car.speed_kmh = car.body.vel.dot(car.body.rot * Vec3::NEG_Z) * 3.6;
    if std::env::var("LIBERTY_DRIVE").is_ok() && (car.age % 1.0) < dt {
        let (y, p, r) = car.body.rot.to_euler(EulerRot::YXZ);
        info!(
            "t {:4.1}s | frame {:5.1} ms | {:6.1} km/h | y {:6.2} | pitch {:5.1} roll {:5.1} yaw {:6.1} | contact {} | comp {:?}",
            car.age,
            dt * 1000.0,
            car.speed_kmh,
            car.body.pos.y,
            p.to_degrees(),
            r.to_degrees(),
            y.to_degrees(),
            car.wheels.iter().filter(|w| w.contact).count(),
            car.wheels.iter().map(|w| (w.compression * 100.0).round() / 100.0).collect::<Vec<_>>()
        );
    }
    // draw between the last two physics steps: 120 Hz steps vs a varying frame rate otherwise judder
    let alpha = (car.accumulator / step).clamp(0.0, 1.0);
    let pos = car.prev.0.lerp(car.body.pos, alpha);
    t.translation = pos;
    t.rotation = car.prev.1.slerp(car.body.rot, alpha);

    // --- wheels: suspension height, roll, steer; right-side meshes face out
    let travel = car.params.susp_upper - car.params.susp_lower;
    for child in children.iter() {
        if let Ok((mut wt, vis)) = wheel_vis.get_mut(child) {
            let w = &car.wheels[vis.idx];
            let offset = carphys::mount(&car.params, w.front) - travel + if w.contact { w.compression } else { 0.0 };
            wt.translation = w.rest + Vec3::Y * offset;
            let face = if w.right { Quat::from_rotation_y(std::f32::consts::PI) } else { Quat::IDENTITY };
            let steer = if w.front { Quat::from_rotation_y(car.steer) } else { Quat::IDENTITY };
            let roll = if w.right { w.spin } else { -w.spin };
            wt.rotation = steer * face * Quat::from_rotation_x(roll);
        }
    }

    // --- chase camera, matched to GTA IV footage: low and close (≈5.8 m back, 1.8 m up), aimed
    // at the car, and swinging round to the direction of travel with a lag so slides show.
    let fwd = car.body.rot * Vec3::NEG_Z;
    let heading = (-fwd.x).atan2(-fwd.z);
    let vel = car.body.vel.with_y(0.0);
    let target = if vel.length() > 4.0 && vel.dot(fwd) > 0.0 { (-vel.x).atan2(-vel.z) } else { heading };
    let mut yaw = car.cam_yaw.unwrap_or(heading);
    let mut d = target - yaw;
    while d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    }
    while d < -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    yaw += d * (dt * 2.8).min(1.0);
    car.cam_yaw = Some(yaw);
    let dir = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
    // rigid behind the car on the lagged heading; only the height is smoothed (bumps)
    let behind = pos - dir * 5.8 + Vec3::Y * 1.8;
    let y = cam_t.translation.y + (behind.y - cam_t.translation.y) * (dt * 8.0).min(1.0);
    cam_t.translation = behind.with_y(if (behind.y - y).abs() < 3.0 { y } else { behind.y });
    let look = pos + Vec3::Y * 0.9 + dir * 1.5;
    cam_t.look_at(look, Vec3::Y);
}

/// Swing the driver's front door on its hinge: `door_open` 0 (shut) .. 1 (fully open), same
/// world-space-delta trick `pedanim` uses for bones (`g_rot.inverse() * turn * g_rot` composed
/// onto the rest local rotation). The driver's side is the car's −X (Bevy); the hinge is at the
/// door's front (leading) edge, so a −70° turn about +Y swings the trailing edge out to −X, away
/// from the body — that's the sign picked below. Not yet wired into `Update`; add
/// `vehicle::animate_doors.after(vehicle::drive)`.
pub fn animate_doors(time: Res<Time>, cars: Query<&Car>, mut joints: Query<&mut Transform, With<CarJoint>>) {
    let test = std::env::var("LIBERTY_DOOR_TEST").is_ok();
    for car in &cars {
        let Some((door_e, rest, g_rot)) = car.door else { continue };
        let open = if test { (time.elapsed_secs() * 0.8).sin() * 0.5 + 0.5 } else { car.door_open };
        let angle = -open * 70.0f32.to_radians();
        let delta = g_rot.inverse() * Quat::from_rotation_y(angle) * g_rot;
        if let Ok(mut t) = joints.get_mut(door_e) {
            t.rotation = rest.rotation * delta;
        }
    }
}

/// A car nobody is driving that still runs its physics: knocked out of traffic, or shoved.
#[derive(Component)]
pub struct Loose;

/// Test hook: give a loose car this velocity on its first physics frame.
#[derive(Component)]
pub struct Launch(pub Vec3);

/// Test hook: a loose car pulls the handbrake once this many seconds have passed (counts down).
#[derive(Component)]
pub struct BrakeAfter(pub f32);

/// Car-to-car contacts, once a frame, between every pair of cars that are close. A traffic car or
/// a parked car that gets hit stops being scripted and becomes `Loose` (real physics).
pub fn car_contacts(
    mut commands: Commands,
    fly: Query<&Fly>,
    mut cars: Query<(Entity, &mut Car, Has<crate::traffic::Traffic>, Has<Loose>)>,
) {
    let _slow = crate::SlowGuard::new("vehicle::car_contacts");
    let driving = fly.single().ok().and_then(|f| f.driving);
    let mut hit: Vec<Entity> = Vec::new();
    let mut pairs = cars.iter_combinations_mut();
    while let Some([(ea, mut a, ta, la), (eb, mut b, tb, lb)]) = pairs.fetch_next() {
        // traffic doesn't yield to traffic at junctions yet: let scripted cars pass through each other
        if (ta && tb) || (a.body.pos - b.body.pos).length() > 9.0 {
            continue;
        }
        let (a, b) = (&mut *a, &mut *b);
        let j = carphys::collide(&mut a.body, &a.params, a.bounds, &mut b.body, &b.params, b.bounds);
        if j > 0.0 {
            for (e, t, l) in [(ea, ta, la), (eb, tb, lb)] {
                if driving != Some(e) && (t || !l) {
                    hit.push(e);
                }
            }
        }
    }
    for e in hit {
        commands.entity(e).remove::<crate::traffic::Traffic>().insert(Loose);
    }
}

/// Step the physics of loose cars (no one at the wheel: no throttle, engine braking holds them).
pub fn loose_cars(
    time: Res<Time>,
    city: Res<City>,
    fly: Query<&Fly>,
    mut cars: Query<(Entity, &mut Transform, &mut Car, Option<&Launch>, Option<&mut BrakeAfter>), With<Loose>>,
    mut commands: Commands,
) {
    let _slow = crate::SlowGuard::new("vehicle::loose_cars");
    let driving = fly.single().ok().and_then(|f| f.driving);
    let dt = time.delta_secs().min(0.1);
    let step = 1.0 / HZ;
    for (e, mut t, mut car, launch, brake_after) in &mut cars {
        if driving == Some(e) {
            continue;
        }
        let car = &mut *car;
        let (input, driving_on) = match brake_after {
            Some(mut b) => {
                b.0 -= dt;
                // braking: the pedal and the handbrake (the handbrake holds the clutch out, so it never reverses)
                (Input { handbrake: b.0 <= 0.0, reverse: if b.0 <= 0.0 { 1.0 } else { 0.0 }, ..Input::default() }, b.0 > 0.0)
            }
            None => (Input::default(), false),
        };
        match launch {
            // a launched car with a brake time is being driven: hold the launch speed until the brakes
            // (the ram test's driver, like IV's car that drove into him)
            Some(Launch(v)) if driving_on => {
                car.body.vel = Vec3::new(v.x, car.body.vel.y, v.z);
            }
            Some(Launch(v)) => {
                car.body.vel = *v;
                commands.entity(e).remove::<Launch>();
            }
            None => {}
        }
        car.accumulator = (car.accumulator + dt).min(step * MAX_STEPS as f32);
        while car.accumulator >= step {
            car.accumulator -= step;
            car.prev = (car.body.pos, car.body.rot);
            if !carphys::step(&mut car.body, &car.params, &mut car.wheels, &car.hull, input, city.collision.values(), step) {
                car.accumulator = 0.0;
                break;
            }
            if launch.is_some() {
                trace_step(car);
            }
        }
        let alpha = (car.accumulator / step).clamp(0.0, 1.0);
        t.translation = car.prev.0.lerp(car.body.pos, alpha);
        t.rotation = car.prev.1.slerp(car.body.rot, alpha);
    }
}
/// Car lights: on after dark for cars that are being driven (traffic, or the player's), off for
/// parked ones; only real lights within 90 m of the camera. The lamp glass glows by the game's
/// `car.headlight.emissive.on / .off`. STAND-INS: "after dark" is the sun's height (when IV
/// switches headlights on isn't measured yet); all of a car's lamp glass glows together (which
/// part is head, tail, brake or indicator isn't decoded), so no brake lights or indicators yet.
pub fn car_lights(
    time: Res<Time>,
    garage: Res<Garage>,
    tc: Option<Res<crate::timecyc::TimeCycle>>,
    fly: Query<(&Transform, &Fly)>,
    mut cars: Query<(Entity, &Transform, &mut CarLights, Option<&crate::traffic::Traffic>, &Children)>,
    mut lamps: Query<&mut Visibility, With<CarLamp>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut timer: Local<f32>,
) {
    let _slow = crate::SlowGuard::new("vehicle::car_lights");
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = 0.25;
    let Ok((cam, fly)) = fly.single() else { return };
    let dark = tc.as_ref().is_some_and(|tc| tc.is_dark());
    let k = tc.as_ref().map_or(0.08, |tc| tc.exposure_k());
    let (glow_on, glow_off) = (garage.visual.get("car.headlight.emissive.on", 10.0), garage.visual.get("car.headlight.emissive.off", 0.05));
    for (e, t, mut lights, traffic, children) in &mut cars {
        let driven = traffic.is_some() || fly.driving == Some(e);
        let on = dark && driven;
        let near = t.translation.distance(cam.translation) < 90.0;
        for c in children.iter() {
            if let Ok(mut v) = lamps.get_mut(c) {
                let want = if on && near { Visibility::Inherited } else { Visibility::Hidden };
                if *v != want {
                    *v = want;
                }
            }
        }
        // emissive isn't scaled by the camera's exposure, so apply IV's exposure here
        let glow = (if on { glow_on } else { glow_off }) * k;
        if on != lights.on || (glow - lights.glow).abs() > 0.05 * glow.max(0.001) {
            lights.on = on;
            lights.glow = glow;
            for h in &lights.glass {
                if let Some(m) = materials.get_mut(h) {
                    m.emissive = LinearRgba::rgb(glow, glow, glow);
                }
            }
        }
    }
}
