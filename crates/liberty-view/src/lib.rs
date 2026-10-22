//! liberty-view — GTA IV from the user's install, drawn by our own renderer.
//!
//!   liberty-view --city manhat02           stream chunks (comma list, `manhat*`, or `city`)
//!   liberty-view [archive.img] [model]     one model (default: manhat02.img 253bway_mh2.wdr)
//!   --shot out.png                         wait until loading settles, save a screenshot, quit
//!   env LIBERTY_CAM_AT=<model> LIBERTY_CAM_YAW=deg LIBERTY_CAM_OFF=dist,height,target_height
//!
//! Controls: right-drag look | WASD move | E/Q up/down | Shift fast | wheel speed | G walk/fly
//!           +/- view distance | Esc quit

pub mod assets;
pub mod carphys;
pub mod collision;
pub mod devui;
pub mod grade;
pub mod hud;
pub mod sky;
pub mod lights;
pub mod looklock;
pub mod ped;
pub mod pedanim;
pub mod peds;
pub mod ragdoll;
pub mod sound;
pub mod stream;
pub mod testlab;
pub mod testpad;
pub mod timecyc;
pub mod traffic;
pub mod vehicle;
pub mod weapon;
pub mod world;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use rage::drawable::{self, Drawable};
use rage::index::GameIndex;
use rage::wtd;
use rage::{Img, Key, Resource as Rsc};

use crate::assets::to_bevy;
use crate::stream::{Cache, City};

const DEFAULT_GAME: &str =
    r"C:\Program Files (x86)\Steam\steamapps\common\Grand Theft Auto IV\GTAIV";
const SKY: Color = Color::srgb(0.62, 0.74, 0.88);

fn game_root() -> PathBuf {
    std::env::var_os("LIBERTY_GAME").map_or_else(|| PathBuf::from(DEFAULT_GAME), PathBuf::from)
}

/// Where the camera starts and what it looks at (Bevy space).
#[derive(Resource)]
pub(crate) struct Start {
    pub(crate) eye: Vec3,
    pub(crate) target: Vec3,
    speed: f32,
}

/// Single-model mode payload.
#[derive(Resource)]
struct OneModel {
    name: String,
    drawable: Drawable,
    textures: Vec<wtd::Texture>,
}

#[derive(Resource)]
struct Water(Vec<rage::water::WaterQuad>);

/// Runs the viewer as the liberty-view exe does.
pub fn run() -> Result<()> {
    run_with(|_| {})
}

/// Runs the viewer, letting another crate add to it: `extend` gets the App once Liberty has set
/// everything up (city or one-model mode, plugins, systems) and just before it runs, to add its own
/// plugins, resources and systems. The modules are public so those systems can read Liberty's
/// state and order themselves against its systems. Not called for --replay, --closedloop, --mini.
pub fn run_with(extend: impl FnOnce(&mut App)) -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--testlab-list") {
        testlab::list();
        return Ok(());
    }
    let mut take = |flag: &str| {
        args.iter().position(|a| a == flag).map(|i| {
            let v = args.get(i + 1).cloned().unwrap_or_default();
            args.drain(i..(i + 2).min(args.len()));
            v
        })
    };
    // the test lab (testlab.rs, re/lab.sh): a station's scripted input, camera and place
    let lab = take("--testlab").map(|s| testlab::apply(&s)).transpose()?;
    let record_secs = take("--record-secs").and_then(|s| s.parse::<f32>().ok()).unwrap_or(6.0);
    let record = take("--record").map(|d| {
        let dir = PathBuf::from(d);
        let _ = std::fs::create_dir_all(&dir);
        (dir, record_secs)
    });
    let shot = take("--shot")
        .map(PathBuf::from)
        .or_else(|| record.as_ref().map(|(d, _)| d.join("unused.png")))
        .map(|path| Shot { path, frame: 0, idle: 0, record: record.clone(), captured: 0 });
    // the dev panel (devui.rs): interactive runs only, or LIBERTY_DEVUI=<panels> for a picture of it
    let devui_list = std::env::var("LIBERTY_DEVUI").ok();
    let devui = (devui_list.is_some() || (shot.is_none() && lab.is_none() && std::env::var("LIBERTY_NO_INPUT").is_err()))
        .then(|| devui::DevUiPlugin::from_list(devui_list.as_deref().unwrap_or("")));
    let city_pattern = take("--city").or_else(|| take("--chunk"));
    let replay = take("--replay");
    let root = game_root();
    let key = Key::from_exe(&root.join("GTAIV.exe"))?;
    if let Some(ep) = replay {
        // S1: drive our ragdoll with IV's recorded body (re/mg2_episode.py), no window
        let d = ped::load_player(&root, &key)?;
        let def = d.ragdoll_def();
        return ragdoll::run_replay(&def, &PathBuf::from(ep));
    }
    if let Some(i) = args.iter().position(|a| a == "--closedloop") {
        let d = ped::load_player(&root, &key)?; // S1: our balancer from IV's live starts, scored against IV
        return ragdoll::run_closed(&d.ragdoll_def(), d.nm_tuning(), &args[i + 1..]);
    }
    if args.iter().any(|a| a == "--mini") {
        return ragdoll::run_mini(ped::load_player(&root, &key)?.ragdoll_def()); // S1: live mini player beside the real game
    }

    let mut app = App::new();
    let title;
    if let Some(pattern) = city_pattern {
        let t0 = Instant::now();
        let index = GameIndex::build(&root, &key)?;
        let names = world::chunk_names(&index, &pattern);
        anyhow::ensure!(!names.is_empty(), "no chunks match {pattern:?}");
        let chunks = names.iter().map(|n| world::plan_chunk(&index, n)).collect::<Result<Vec<_>>>()?;
        info!(
            "planned {} chunks ({} objects, {} stand-ins) in {:.1}s",
            chunks.len(),
            chunks.iter().map(|c| c.hd.len()).sum::<usize>(),
            chunks.iter().map(|c| c.lods.len()).sum::<usize>(),
            t0.elapsed().as_secs_f32()
        );
        if std::env::var("LIBERTY_DUMP_PLACEMENTS").is_ok() {
            return Ok(()); // plan_chunk wrote the placement CSVs; no window needed
        }
        let start = city_start(&index, &chunks);
        let mut water = rage::water::read_water(&std::fs::read_to_string(root.join("common/data/water.dat"))?)?;
        water.extend(testpad::lab_water());
        title = pattern.clone();
        let garage = vehicle::Garage::open(&root, &key)?;
        let roads = traffic::Roads::load(&root, &key, &garage)?;
        app.insert_resource(City::new(index, chunks))
            .insert_resource(Cache::default())
            .insert_resource(start)
            .insert_resource(Water(water))
            .insert_resource(garage)
            .insert_resource(roads)
            .insert_resource(ped::PlayerRes(ped::load_player(&root, &key)?))
            .insert_resource(weapon::load(&root, &key)?)
            .init_resource::<weapon::Armed>()
            .add_systems(Startup, (ped::spawn_player, weapon::spawn_held.after(ped::spawn_player)))
            .insert_resource(hud::HudSource { root: root.clone(), key: key.clone() })
            .add_systems(Startup, hud::setup)
            .add_systems(Update, (hud::weapon_hud, hud::radar).after(ped::control_player))
            .add_event::<weapon::BulletHit>()
            .add_event::<sound::PlaySfx>()
            .add_event::<sound::Honk>()
            .init_resource::<sound::Footsteps>()
            .init_resource::<sound::Engine>()
            .insert_resource(sound::Ambience::load(&root)?)
            .add_systems(Update, sound::ambience.before(sound::play))
            .add_systems(Update, sound::engine.after(vehicle::drive).before(sound::play))
            .insert_resource(sound::Sfx::load(&root, &key)?)
            .add_systems(Update, (sound::triggers, sound::play).chain().after(weapon::fire).after(ped::control_player))
            .add_systems(Update, (weapon::select, weapon::show_held, weapon::fire, weapon::fx_decay, weapon::debug_barrel).chain())
            // after fly_camera: it sets the camera's rotation every frame, and Niko's orbit camera
            // has to have the last word (unordered, Bevy may run them either way round)
            .add_systems(Update, ped::control_player.after(vehicle::drive).after(fly_camera))
            .insert_resource(peds::PedPool::open(&root, &key)?)
            .insert_resource(peds::Nav::open(&root, &key))
            .init_resource::<peds::PedsOn>()
            .insert_resource(timecyc::TimeCycle::load(&root)?)
            .add_systems(Update, (timecyc::advance_time, timecyc::apply_time, timecyc::apply_glow).chain())
            .add_systems(Startup, (lights::setup, lights::iv_falloff, sky::setup))
            .add_systems(Update, sky::update.after(timecyc::apply_time))
            .init_resource::<sound::TrafficSound>()
            .add_systems(Update, (sound::traffic_engines.after(traffic::move_traffic), sound::fade_out))
            .add_systems(Update, (lights::lamps, lights::signals, lights::face_camera))
            .add_systems(Update, (peds::spawn_peds, peds::move_peds).chain())
            .add_systems(Update, (vehicle::car_controls, vehicle::drive, vehicle::animate_doors).chain())
            .add_systems(Update, vehicle::car_lights)
            .add_systems(Update, (traffic::spawn_traffic, traffic::move_traffic, vehicle::car_contacts, vehicle::loose_cars).chain().after(vehicle::drive))
            .add_systems(Update, traffic::probe_roads)
            .add_systems(Startup, spawn_water)
            .add_systems(Startup, testpad::spawn_testpad)
            .add_systems(Update, (stream::stream, stream::distance_bands, view_distance_keys));
        if let Some(lab) = lab {
            app.insert_resource(lab)
                .add_systems(Update, testlab::probe.after(ped::control_player).after(peds::move_peds).after(vehicle::drive))
                .add_systems(Last, testlab::write);
        }
    } else {
        let img_rel = args.first().map_or("pc/data/maps/manhat/manhat02.img", String::as_str);
        let model = args.get(1).map_or("253bway_mh2.wdr", String::as_str);
        let one = load_one(&root, &key, img_rel, model)?;
        let (a, b) = (Vec3::from(to_bevy(one.drawable.bounds_min)), Vec3::from(to_bevy(one.drawable.bounds_max)));
        let (center, size) = ((a + b) / 2.0, (b - a).abs().max_element().max(2.0));
        let (s, c) = cam_yaw().sin_cos();
        let eye = center + Vec3::new(s, 0.0, c) * size * 1.5 + Vec3::Y * size * 0.25;
        title = model.to_string();
        app.insert_resource(Start { eye, target: center, speed: (size / 4.0).max(5.0) })
            .insert_resource(one)
            .add_systems(Startup, spawn_one);
    }
    if let Some(shot) = shot {
        app.insert_resource(shot).add_systems(Update, take_shot);
    }
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        // LIBERTY_WINDOW_AT=x,y (desktop pixels): where the window opens, e.g. on another monitor
        primary_window: Some(Window {
            title: format!("Liberty — {title}"),
            // LIBERTY_HIDDEN (scripted test runs): no window on screen at all, so it can't take the
            // keyboard or cover anything; it still renders and records. (Bevy 0.16 doesn't pass
            // `focused: false` on to winit, so an unfocused visible window isn't available.)
            // The file %LOCALAPPDATA%\Liberty\show_runs (the player: "let me watch") overrides it for every run.
            visible: std::env::var("LIBERTY_HIDDEN").is_err() || show_runs(),
            position: std::env::var("LIBERTY_WINDOW_AT")
                .ok()
                .and_then(|v| v.split_once(',').and_then(|(x, y)| Some(IVec2::new(x.trim().parse().ok()?, y.trim().parse().ok()?))))
                .map_or(WindowPosition::Automatic, WindowPosition::At),
            ..default()
        }),
        ..default()
    }).set(bevy::log::LogPlugin { custom_layer: devui::log_layer, ..default() }))
    .add_systems(PreUpdate, ignore_input.after(bevy::input::InputSystem))
    .add_plugins(looklock::plugin)
    // LIBERTY_FIXED_DT (scripted tests): every frame advances the game clock by exactly 1/60 s,
    // however long it really took, so a run plays out the same way each time (shove timing,
    // animation phase, physics steps) instead of depending on the PC's frame times
    .insert_resource(if std::env::var("LIBERTY_FIXED_DT").is_ok() {
        bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_secs_f64(1.0 / 60.0))
    } else {
        bevy::time::TimeUpdateStrategy::Automatic
    })
    .add_plugins(FrameTimeDiagnosticsPlugin::default())
    .add_plugins(grade::GradePlugin)
    .add_plugins(sky::SkyPlugin)
    .insert_resource(ClearColor(SKY))
    .insert_resource(AmbientLight { brightness: 600.0, ..default() })
    .add_systems(Startup, setup_scene)
    .add_systems(Update, (fly_camera, quit_on_esc, hud, display_keys, stall_watch));
    if let Some(devui) = devui {
        app.add_plugins(devui);
    }
    extend(&mut app);
    app.run();
    Ok(())
}

fn cam_yaw() -> f32 {
    std::env::var("LIBERTY_CAM_YAW").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(35.0).to_radians()
}

/// Start next to LIBERTY_CAM_AT if placed, else above the first chunk. Default: the tennis courts
/// in Meadows Park (open ground, nothing overhead).
fn city_start(index: &GameIndex, chunks: &[world::ChunkPlan]) -> Start {
    let name = std::env::var("LIBERTY_CAM_AT").unwrap_or_else(|_| "am_fm2_tennis".into());
    let want = rage::placement::hash(name.trim_end_matches(".wdr"));
    // "@x,y,z" = a raw GTA position (as the HUD prints it) instead of a model name
    let raw = name.strip_prefix('@').map(|s| s.split(',').filter_map(|v| v.trim().parse::<f32>().ok()).collect::<Vec<_>>());
    let at = match raw {
        _ if testpad::enabled() => Some(testpad::centre()),
        Some(v) if v.len() == 3 => Some(Vec3::from(assets::to_bevy([v[0], v[1], v[2]]))),
        _ => chunks.iter().flat_map(|c| c.hd.iter()).find(|p| p.model == want).map(|p| p.transform.translation),
    };
    // on the test pad, and when starting on foot, the camera starts close: Niko appears on the
    // ground under it
    let default_off = if testpad::enabled() { "6,3,1" } else if std::env::var("LIBERTY_WALK").is_ok_and(|v| v != "0") { "8,3,1" } else { "" };
    let off: Vec<f32> =
        std::env::var("LIBERTY_CAM_OFF").unwrap_or_else(|_| default_off.into()).split(',').filter_map(|v| v.parse().ok()).collect();
    let (s, c) = cam_yaw().sin_cos();
    let dir = Vec3::new(s, 0.0, c);
    match at {
        Some(at) => {
            let (dist, h, th) = (off.first().copied().unwrap_or(140.0), off.get(1).copied().unwrap_or(70.0), off.get(2).copied().unwrap_or(0.0));
            Start { eye: at + dir * dist + Vec3::Y * h, target: at + Vec3::Y * th, speed: 25.0 }
        }
        None => {
            let c0 = &chunks[0];
            let _ = index;
            Start { eye: c0.center + dir * c0.radius + Vec3::Y * 150.0, target: c0.center, speed: 40.0 }
        }
    }
}

fn load_one(root: &std::path::Path, key: &Key, img_rel: &str, model: &str) -> Result<OneModel> {
    let img = Img::open(&root.join(img_rel), key)?;
    let entry = img.find(model).with_context(|| format!("{model} not in {img_rel}"))?;
    let res = Rsc::load(&img.read(entry)?)?;
    let drawable = if entry.ext() == "wft" { drawable::read_fragment(&res)? } else { drawable::read(&res)? };
    // every pack in the same archive, as one flat pool, plus the model's own
    let mut textures = drawable.textures.clone();
    for e in img.entries.iter().filter(|e| e.ext() == "wtd") {
        textures.extend(wtd::read(&Rsc::load(&img.read(e)?)?)?);
    }
    // vehicles share vehicle_generic_* textures from a loose pack
    let shared = root.join("pc/textures/vehicle.wtd");
    if shared.exists() {
        textures.extend(wtd::read(&Rsc::load(&std::fs::read(shared)?)?)?);
    }
    Ok(OneModel { name: model.to_string(), drawable, textures })
}

fn spawn_one(
    mut commands: Commands,
    one: Res<OneModel>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let d = &one.drawable;
    let mats: Vec<Handle<StandardMaterial>> = d
        .shaders
        .iter()
        .map(|s| {
            let t = s.diffuse.as_ref().and_then(|n| one.textures.iter().find(|t| t.name.eq_ignore_ascii_case(n)));
            let alpha = match t.map(|t| t.format) {
                Some(wtd::Format::Dxt3 | wtd::Format::Dxt5) => AlphaMode::Mask(0.5),
                _ => AlphaMode::Opaque,
            };
            materials.add(assets::material(t.map(|t| images.add(assets::gpu_image(t))), alpha))
        })
        .collect();
    let fallback = materials.add(assets::material(None, AlphaMode::Opaque));
    for m in &d.meshes {
        commands.spawn((
            Mesh3d(meshes.add(assets::build_mesh(m))),
            MeshMaterial3d(mats.get(m.shader).cloned().unwrap_or_else(|| fallback.clone())),
        ));
    }
    info!("{}: {} meshes", one.name, d.meshes.len());
}

fn spawn_water(
    mut commands: Commands,
    water: Res<Water>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    timecycle: Option<ResMut<timecyc::TimeCycle>>,
) {
    // water.dat stops at ±3 km; ring it with open sea out to the horizon (not one plane under
    // everything — that would cut through the subway tunnels below sea level).
    let rect = |x1: f32, y1: f32, x2: f32, y2: f32| rage::water::WaterQuad {
        corners: [[x1, y1, 0.0], [x2, y1, 0.0], [x1, y2, 0.0], [x2, y2, 0.0]],
        params: [[0.0; 4]; 4],
        kind: 0,
        flag: 0.0,
    };
    const FAR: f32 = 40_000.0;
    const EDGE: f32 = 3_000.0;
    let mut quads = water.0.clone();
    quads.extend([
        rect(-FAR, -FAR, FAR, -EDGE),
        rect(-FAR, EDGE, FAR, FAR),
        rect(-FAR, -EDGE, -EDGE, EDGE),
        rect(EDGE, -EDGE, FAR, EDGE),
    ]);
    let mat = materials.add(StandardMaterial {
        base_color: Color::srgba(0.10, 0.29, 0.36, 0.92),
        // Lit PBR water with no reflection probe washes out to grey; unlit keeps the colour
        // until there's a proper water shader.
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 0.3,
        reflectance: 0.35,
        cull_mode: None,
        double_sided: true,
        ..default()
    });
    if let Some(mut tc) = timecycle {
        tc.water = Some(mat.clone()); // the time of day dims it at night
    }
    commands.spawn((Mesh3d(meshes.add(assets::water_mesh(&quads))), MeshMaterial3d(mat), Transform::default()));
}

#[derive(Component)]
pub struct Fly {
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) speed: f32,
    /// F toggles: fly freely, or walk on the collision with gravity.
    pub walking: bool,
    pub vel_y: f32,
    /// The car being driven (camera follows it; see vehicle.rs).
    pub driving: Option<Entity>,
}

#[derive(Component)]
pub(crate) struct HudText;

fn setup_scene(mut commands: Commands, start: Res<Start>) {
    let look = Transform::from_translation(start.eye).looking_at(start.target, Vec3::Y);
    let (yaw, pitch, _) = look.rotation.to_euler(EulerRot::YXZ);
    commands.spawn((
        Camera3d::default(),
        // HDR so bright things can bloom; the grading pass (grade.rs) runs on the finished frame
        Camera { hdr: true, ..default() },
        // IV has no filmic curve: the lit picture times its exposure goes to the grading as it is
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        bevy::render::camera::Exposure::default(),
        bevy::core_pipeline::bloom::Bloom::NATURAL,
        grade::Grade::NEUTRAL,
        // ears, for the sounds that have a place in the world (traffic)
        bevy::audio::SpatialListener::new(0.4),
        Projection::Perspective(PerspectiveProjection { far: 6000.0, ..default() }),
        DistanceFog { color: SKY, falloff: FogFalloff::Linear { start: 1200.0, end: 4500.0 }, ..default() },
        look,
        Fly { yaw, pitch, speed: start.speed, walking: std::env::var("LIBERTY_WALK").is_ok_and(|v| v != "0"), vel_y: 0.0, driving: None },
    ));
    commands.spawn((
        DirectionalLight { illuminance: 9_000.0, shadows_enabled: std::env::var("LIBERTY_NO_SHADOWS").is_err(), ..default() },
        Transform::from_xyz(1.0, 2.0, 0.6).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        HudText,
        Text::new(""),
        TextFont { font_size: 15.0, ..default() },
        Node { position_type: PositionType::Absolute, top: Val::Px(8.0), left: Val::Px(10.0), ..default() },
    ));
}

fn hud(
    mut text: Query<&mut Text, With<HudText>>,
    city: Option<Res<City>>,
    diag: Res<DiagnosticsStore>,
    shown: Query<&ViewVisibility, With<Mesh3d>>,
    cam: Query<&Transform, With<Camera3d>>,
    fly: Query<&Fly>,
    cars: Query<&vehicle::Car>,
    tc: Option<Res<timecyc::TimeCycle>>,
    armed: Option<Res<weapon::Armed>>,
    weapons: Option<Res<weapon::Weapons>>,
    mut frame: Local<u32>,
) {
    *frame += 1;
    if *frame % 15 != 0 {
        return;
    }
    let Ok(mut text) = text.single_mut() else { return };
    let fps = diag.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
    let drawn = shown.iter().filter(|v| v.get()).count();
    let mode = match fly.single() {
        Ok(f) if f.driving.is_some() => format!("DRIVE {}", f.driving.and_then(|c| cars.get(c).ok()).map_or("", |c| c.model.as_str())),
        Ok(f) if f.walking => "WALK".into(),
        _ => "FLY".into(),
    };
    let mut s = format!("{mode} | {fps:.0} fps | {drawn} meshes drawn");
    if let Some(tc) = tc {
        let h = tc.hour;
        s += &format!(" | {:02}:{:02} {}", h as u32, ((h.fract()) * 60.0) as u32, tc.weather_name());
    }
    if let Some((name, left, size, reloading)) = armed.as_ref().zip(weapons.as_ref()).and_then(|(a, w)| weapon::ammo(a, w)) {
        s += &format!(" | {name} {left}/{size}{}", if reloading { " reloading" } else { "" });
    }
    if let Some(city) = city {
        let p = cam.single().map(|t| t.translation).unwrap_or_default();
        s += &format!(
            " | {}/{} chunks loaded | view x{:.2}{} | at {:.0}, {:.0}, {:.0} (GTA)",
            city.loaded(),
            city.chunks.len(),
            city.view,
            if city.idle(p) { "" } else { " | loading..." },
            p.x,
            -p.z,
            p.y
        );
    }
    s += "\nRight-drag look, middle mouse locks it | WASD move | E/Q up/down | Shift fast | wheel speed | G walk/fly | Space jump | F enter/exit car | 1-9 gun, 0 away | right mouse aim, left fire, R reload | C spawn car | H horn | [ ] change car | R flip | T +1 hour | Y weather | +/- view distance | F1 hide text | F11 fullscreen | Esc quit";
    text.0 = s;
}

/// LIBERTY_NO_INPUT: scripted test runs ignore the keyboard and mouse, so whatever is being typed
/// or played when the window pops up can't steer the test.
fn ignore_input(
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    mut motion: ResMut<AccumulatedMouseMotion>,
    mut scroll: ResMut<AccumulatedMouseScroll>,
) {
    if std::env::var("LIBERTY_NO_INPUT").is_err() {
        return;
    }
    keys.reset_all();
    buttons.reset_all();
    motion.delta = Vec2::ZERO;
    scroll.delta = Vec2::ZERO;
}

fn view_distance_keys(keys: Res<ButtonInput<KeyCode>>, mut city: ResMut<City>) {
    if keys.just_pressed(KeyCode::Equal) || keys.just_pressed(KeyCode::NumpadAdd) {
        city.view = (city.view * 1.25).min(4.0);
    }
    if keys.just_pressed(KeyCode::Minus) || keys.just_pressed(KeyCode::NumpadSubtract) {
        city.view = (city.view / 1.25).max(0.3);
    }
}

/// Eye height above the feet when walking (metres).
const EYE: f32 = 1.7;

fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    city: Option<Res<City>>,
    mut cam: Query<(&mut Transform, &mut Fly)>,
) {
    let Ok((mut t, mut fly)) = cam.single_mut() else { return };
    if fly.driving.is_some() {
        return; // vehicle.rs drives the camera
    }
    if looklock::looking(&buttons) {
        fly.yaw -= motion.delta.x * 0.003;
        fly.pitch = (fly.pitch - motion.delta.y * 0.003).clamp(-1.55, 1.55);
    }
    if scroll.delta.y != 0.0 {
        fly.speed = (fly.speed * 1.2f32.powf(scroll.delta.y)).clamp(0.5, 500.0);
    }
    t.rotation = Quat::from_euler(EulerRot::YXZ, fly.yaw, fly.pitch, 0.0);
    let dt = time.delta_secs().min(0.05);

    if keys.just_pressed(KeyCode::KeyG) && city.is_some() {
        fly.walking = !fly.walking;
        fly.vel_y = 0.0;
    }

    if fly.walking && city.is_some() {
        return; // Niko (ped.rs) moves and owns the camera
    }
    if !fly.walking {
        // LIBERTY_FLY_TO=x,y,z,speed (GTA metres, m/s): fly straight there (scripted streaming test)
        let fly_to: Option<Vec<f32>> = std::env::var("LIBERTY_FLY_TO").ok().map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect());
        if let Some(v) = fly_to.filter(|v| v.len() == 4) {
            let target = Vec3::new(v[0], v[2], -v[1]);
            let to = target - t.translation;
            let step = v[3] * dt;
            t.translation = if to.length() > step { t.translation + to.normalize() * step } else { target };
            return;
        }
        let mut dir = Vec3::ZERO;
        for (key, v) in [
            (KeyCode::KeyW, *t.forward()),
            (KeyCode::KeyS, *t.back()),
            (KeyCode::KeyA, *t.left()),
            (KeyCode::KeyD, *t.right()),
            (KeyCode::KeyE, Vec3::Y),
            (KeyCode::KeyQ, Vec3::NEG_Y),
        ] {
            if keys.pressed(key) {
                dir += v;
            }
        }
        let boost = if keys.pressed(KeyCode::ShiftLeft) { 4.0 } else { 1.0 };
        t.translation += dir.normalize_or_zero() * fly.speed * boost * dt;
        return;
    }

    // Walking: move on the ground plane, slide along walls, fall, step up kerbs/stairs.
    let Some(city) = city else { return };
    let chunks = city.collision.values();
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).normalize_or_zero();
    let mut dir = Vec3::ZERO;
    for (key, v) in [
        (KeyCode::KeyW, flat(*t.forward())),
        (KeyCode::KeyS, -flat(*t.forward())),
        (KeyCode::KeyA, flat(*t.left())),
        (KeyCode::KeyD, -flat(*t.left())),
    ] {
        if keys.pressed(key) {
            dir += v;
        }
    }
    // LIBERTY_AUTOWALK=<seconds>: hold W for that long (scripted wall/ground test for --shot)
    if std::env::var("LIBERTY_AUTOWALK").ok().and_then(|s| s.parse::<f32>().ok()).is_some_and(|s| time.elapsed_secs() < s) {
        dir += flat(*t.forward());
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) { 9.0 } else { 4.0 };
    let feet = t.translation - Vec3::Y * EYE;
    let mut next = feet + dir.normalize_or_zero() * speed * dt;
    next = collision::push_out_of_walls(chunks.clone(), next, 0.35, &[0.55, 1.2, 1.6]);

    // Is there any collision under us at all? If not (chunk not streamed yet), hover.
    let Some(_) = collision::ground_height(chunks.clone(), next.x, next.z, next.y + 500.0) else {
        t.translation = next + Vec3::Y * EYE;
        return;
    };
    const STEP: f32 = 0.6;
    let ground = collision::ground_height(chunks.clone(), next.x, next.z, feet.y + STEP);
    let on_ground = ground.is_some_and(|g| feet.y - g < 0.05);
    if on_ground && keys.just_pressed(KeyCode::Space) {
        fly.vel_y = 6.0;
    }
    fly.vel_y -= 20.0 * dt;
    next.y = feet.y + fly.vel_y * dt;
    if let Some(g) = ground {
        if next.y <= g {
            next.y = g;
            fly.vel_y = 0.0;
        }
    }
    t.translation = next + Vec3::Y * EYE;
}
/// Logs a system that ran over 0.3 s (wall clock) when it goes out of scope: put
/// `let _slow = crate::SlowGuard::new("name");` at the top of a system to find what stalls a frame.
pub struct SlowGuard(&'static str, std::time::Instant);

impl SlowGuard {
    pub fn new(name: &'static str) -> SlowGuard {
        SlowGuard(name, std::time::Instant::now())
    }
}

impl Drop for SlowGuard {
    fn drop(&mut self) {
        let took = self.1.elapsed().as_secs_f32();
        if took > 0.3 {
            warn!("slow system: {} took {took:.2} s", self.0);
        }
    }
}

/// A frame that took over half a second (wall clock) goes in the log with where the camera was, so
/// a freeze can be traced to a place.
fn stall_watch(mut last: Local<Option<std::time::Instant>>, cam: Query<&Transform, With<Camera3d>>) {
    let now = std::time::Instant::now();
    if let Some(prev) = *last {
        let gap = now.duration_since(prev).as_secs_f32();
        if gap > 0.5 {
            let at = cam.single().map(|t| t.translation).unwrap_or_default();
            warn!("slow frame: {gap:.2} s, camera at GTA ({:.0}, {:.0}, {:.0})", at.x, -at.z, at.y);
        }
    }
    *last = Some(now);
}

/// F1 hides/shows the HUD text; F11 toggles borderless fullscreen (cursor hidden) for clean recordings.
fn display_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut hud: Query<&mut Visibility, With<HudText>>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    devui: Option<Res<devui::DevUi>>,
) {
    // with the dev panel, F1 opens its Controls instead
    if keys.just_pressed(KeyCode::F1) && devui.is_none() {
        for mut v in &mut hud {
            *v = if *v == Visibility::Hidden { Visibility::Inherited } else { Visibility::Hidden };
        }
    }
    if keys.just_pressed(KeyCode::F11) {
        if let Ok(mut w) = windows.single_mut() {
            let full = matches!(w.mode, bevy::window::WindowMode::Windowed);
            w.mode = if full {
                bevy::window::WindowMode::BorderlessFullscreen(bevy::window::MonitorSelection::Current)
            } else {
                bevy::window::WindowMode::Windowed
            };
            w.cursor_options.visible = !full;
        }
    }
}

/// Esc quits at once in scripted and --shot runs; the dev panel asks first.
fn quit_on_esc(keys: Res<ButtonInput<KeyCode>>, mut exit: EventWriter<AppExit>, devui: Option<Res<devui::DevUi>>) {
    if keys.just_pressed(KeyCode::Escape) && devui.is_none() {
        exit.write(AppExit::Success);
    }
}

/// `--shot <file.png>`: once loading has settled, save a screenshot and quit.
/// `--record <dir>` instead: once settled, save every 2nd frame for `--record-secs` (default 6)
/// as <dir>/f00000.png…, then quit — ffmpeg turns them into a video to watch.
#[derive(Resource)]
struct Shot {
    path: PathBuf,
    frame: u32,
    idle: u32,
    record: Option<(PathBuf, f32)>,
    captured: u32,
}

/// When recording started (scripted input in ped.rs counts from here).
#[derive(Resource, Clone, Copy)]
pub struct RecordStart(pub f32);

#[allow(clippy::too_many_arguments)]
fn take_shot(
    mut commands: Commands,
    time: Res<Time>,
    mut shot: ResMut<Shot>,
    city: Option<Res<City>>,
    started: Option<Res<RecordStart>>,
    cam: Query<&Transform, With<Camera3d>>,
    mut exit: EventWriter<AppExit>,
) {
    shot.frame += 1;
    let settled = match (&city, cam.single()) {
        (Some(c), Ok(t)) => c.idle(t.translation),
        _ => true,
    };
    shot.idle = if settled || started.is_some() { shot.idle + 1 } else { 0 };
    if let Some((dir, secs)) = shot.record.clone() {
        if shot.idle == 90 {
            commands.insert_resource(RecordStart(time.elapsed_secs()));
        }
        if let Some(s) = started {
            if time.elapsed_secs() - s.0 > secs {
                info!("recorded {} frames over {secs:.1}s", shot.captured);
                exit.write(AppExit::Success);
            } else if shot.frame % 2 == 0 {
                let path = dir.join(format!("f{:05}.png", shot.captured));
                shot.captured += 1;
                commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
            }
        }
        return;
    }
    if shot.idle == 90 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(shot.path.clone()));
    }
    if shot.idle == 120 || shot.frame > 20_000 {
        exit.write(AppExit::Success);
    }
}

/// Scripted runs open their window anyway while `%LOCALAPPDATA%\Liberty\show_runs` exists.
fn show_runs() -> bool {
    std::env::var_os("LOCALAPPDATA").is_some_and(|d| std::path::Path::new(&d).join("Liberty").join("show_runs").exists())
}
