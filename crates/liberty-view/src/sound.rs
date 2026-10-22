//! Sound: the game's own samples (`pc/audio/sfx`), picked the way its own tables say
//! (`sounds.dat15`, `game.dat16`; see `rage::audio`). So far: gunshots, bullet impacts and Niko's
//! footsteps, the engine of the car being driven, traffic engines and horns, ambient one-shots.
//! Sounds placed in the world (`PlaySfx::at`, traffic loops) fade with distance the game's way: its
//! DEFAULT_ROLLOFF curve over the distance divided by the sound's category rolloff scale
//! (`rage::audio::{default_rolloff_db, Categories}`), panned from the camera. One-shots fade in over
//! their own attack; loops being stopped fade out over their release (`FadeOut`). Not done: IV's
//! distance filtering (DISTANCE_TO_FILTER_CUTOFF) and reverb, doppler, ambient beds and emitters.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use bevy::audio::Volume;
use bevy::prelude::*;
use rage::Key;
use rage::audio::{Banks, Sounds};
use rage::placement::hash;

use crate::Fly;
use crate::vehicle::Car;
use crate::weapon::{Armed, BulletHit, Weapons};

/// Play a sound from sounds.dat15, by the hash of its name.
#[derive(Event)]
pub struct PlaySfx {
    pub sound: u32,
    pub volume: f32,
    /// -1 left .. 1 right (0 for everything that isn't placed around the listener).
    pub pan: f32,
    /// Where it happens: then the volume falls off with the distance from the camera and the pan
    /// comes from the direction (`pan` is ignored).
    pub at: Option<Vec3>,
}

/// A traffic driver sounds his horn (his car model's own), somewhere in the world.
#[derive(Event)]
pub struct Honk {
    pub model: String,
    pub at: Vec3,
}

/// Niko's steps, counted by ped.rs as his feet come down; `running` picks the run sample.
#[derive(Resource, Default)]
pub struct Footsteps {
    pub count: u32,
    pub running: bool,
}

#[derive(Resource)]
pub struct Sfx {
    sounds: Sounds,
    /// game.dat16: each object's sound references, in order.
    game: HashMap<String, Vec<u32>>,
    /// A car model's audio object is named by its vehicles.ide game name (perennial2 -> PEREN2,
    /// moonbeam -> MOONB), not its model name: model -> game name.
    car_objects: HashMap<String, String>,
    banks: Banks,
    cats: rage::audio::Categories,
    seed: u32,
    /// Scripted test runs (LIBERTY_NO_INPUT) stay silent; LIBERTY_SOUND_DEBUG logs what would play.
    muted: bool,
    /// LIBERTY_SOUND_VOLUME=<0..1> scales everything (and un-mutes a scripted run: 0 exercises
    /// the whole playback path without a sound).
    volume: f32,
}

/// Over everything: the mixes come out near full scale.
const MASTER: f32 = 0.6;
/// The leather-shoe set in a FOOTSTEPS_* object: five sounds per shoe type (heel, toe, scuff, run,
/// jump landing), shoe types in the order high heels, leather, rubber, ..., boots.
const SHOE_LEATHER: usize = 5;

impl Sfx {
    pub fn load(root: &Path, key: &Key) -> Result<Sfx> {
        let sounds = Sounds::load(root)?;
        let game = rage::audio::game_objects(root, &sounds)?;
        let volume: Option<f32> = std::env::var("LIBERTY_SOUND_VOLUME").ok().and_then(|v| v.parse().ok());
        let ide = std::fs::read_to_string(root.join("common/data/vehicles.ide")).unwrap_or_default();
        let car_objects = rage::vehicles::read_vehicles_ide(&ide)
            .unwrap_or_default()
            .into_iter()
            .map(|d| (d.model.to_ascii_lowercase(), d.game_name.to_ascii_uppercase()))
            .collect();
        Ok(Sfx {
            sounds,
            game,
            car_objects,
            banks: Banks::new(root, key),
            cats: rage::audio::Categories::load(root)?,
            seed: 0x2545_F491,
            muted: std::env::var("LIBERTY_NO_INPUT").is_ok() && volume.is_none(),
            volume: volume.unwrap_or(1.0),
        })
    }

    /// The reload sounds of a weapon's animation group (`gun@handgun` -> AUD_EVENT_HANDGUN_RELOAD_*),
    /// in the order a reload goes: clip out, clip in, cock, release.
    fn reload_events(&self, anim_group: &str) -> Vec<u32> {
        let gun = anim_group.trim_start_matches("gun@").to_ascii_uppercase();
        let prefix = format!("AUD_EVENT_{gun}_RELOAD_");
        let rank = |n: &str| {
            if n.contains("REMOVE") || n.contains("DETATCH") {
                0
            } else if n.contains("INSERT") || n.contains("ATTACH") {
                1
            } else if n.contains("RELEASE") || n.contains("SLAM") || n.contains("SLIDE") {
                3
            } else {
                2
            }
        };
        let mut found: Vec<(i32, &str, u32)> =
            self.sounds.sounds.iter().filter(|(_, s)| s.name.starts_with(&prefix)).map(|(h, s)| (rank(&s.name), s.name.as_str(), *h)).collect();
        found.sort();
        found.into_iter().map(|f| f.2).collect()
    }

    /// How loud a sound is from `distance` m away (linear): the game's DEFAULT_ROLLOFF curve over
    /// the distance divided by its category's rolloff scale (sirens 12, guns 9, engines 3 with BASE's).
    fn distance_gain(&self, sound: u32, distance: f32) -> f32 {
        let scale = self.sounds.inherited(sound, &|s| s.category).map_or(1.0, |c| self.cats.rolloff(c));
        10f32.powf(rage::audio::default_rolloff_db(distance / scale.max(0.01)) / 20.0)
    }

    /// The n-th sound of a game object (`W_GLOCK`, `FOOTSTEPS_CONCRETE_CLEAN`, ...).
    fn of(&self, object: &str, n: usize) -> Option<u32> {
        self.game.get(&object.to_ascii_uppercase())?.get(n).copied()
    }
}

/// Turn what happened this frame into sounds: a shot, a bullet landing, a footstep.
pub fn triggers(
    sfx: Res<Sfx>,
    armed: Res<Armed>,
    weapons: Res<Weapons>,
    steps: Res<Footsteps>,
    mut hits: EventReader<BulletHit>,
    mut play: EventWriter<PlaySfx>,
    mut seen: Local<(u32, u32)>,
    mut reload_was: Local<f32>,
    mut honks: EventReader<Honk>,
) {
    let _slow = crate::SlowGuard::new("sound::triggers");
    // traffic horns: placed where the car is (distance and pan in `play`)
    for h in honks.read() {
        let Some(sound) = sfx.car_sound(&h.model, "HORN") else { continue };
        play.write(PlaySfx { sound, volume: 0.8, pan: 0.0, at: Some(h.at) });
    }
    // a reload: the weapon's AUD_EVENT_<GUN>_RELOAD_* sounds (clip out, clip in, cock). In the game
    // the reload animation's own event track times them; that track isn't decoded, so they are
    // spread evenly through the reload.
    let now = armed.reload_progress().map_or(-1.0, |(t, total)| t / total.max(0.01));
    if now >= 0.0 {
        if let Some(i) = armed.weapon {
            let events = sfx.reload_events(&weapons.0[i].info.anim_group);
            for (k, &sound) in events.iter().enumerate() {
                let at = 0.2 + 0.65 * k as f32 / events.len().max(2).saturating_sub(1) as f32;
                if *reload_was < at && now >= at {
                    play.write(PlaySfx { sound, volume: 0.8, pan: 0.0, at: None });
                }
            }
        }
    }
    *reload_was = now;
    // a shot: the weapon model's own audio object, first sound = fire
    if armed.shots != seen.0 {
        seen.0 = armed.shots;
        if let Some(sound) = armed.weapon.and_then(|i| sfx.of(&weapons.0[i].info.model, 0)) {
            play.write(PlaySfx { sound, volume: 1.0, pan: 0.0, at: None });
        }
    }
    for h in hits.read() {
        if h.entity.is_none() {
            play.write(PlaySfx { sound: hash("BULLET_IMPACT_GENERIC"), volume: 0.5, pan: 0.0, at: None });
        }
    }
    // a step: concrete until the collision materials are read, leather shoes
    if steps.count != seen.1 {
        seen.1 = steps.count;
        let which = SHOE_LEATHER + if steps.running { 3 } else { 0 };
        if let Some(sound) = sfx.of("FOOTSTEPS_CONCRETE_CLEAN", which) {
            play.write(PlaySfx { sound, volume: 0.45, pan: 0.0, at: None });
        }
    }
}

/// Mix each requested sound from its layers and play it.
pub fn play(
    mut commands: Commands,
    mut sfx: ResMut<Sfx>,
    mut events: EventReader<PlaySfx>,
    mut sources: ResMut<Assets<AudioSource>>,
    cam: Query<&GlobalTransform, With<Camera3d>>,
) {
    let _slow = crate::SlowGuard::new("sound::play");
    for e in events.read() {
        let (mut gain, mut pan) = (e.volume, e.pan);
        if let (Some(at), Ok(c)) = (e.at, cam.single()) {
            let to = at - c.translation();
            gain *= sfx.distance_gain(e.sound, to.length());
            pan = to.normalize_or_zero().dot(c.right().as_vec3()).clamp(-1.0, 1.0);
        }
        // past the end of its rolloff (-100 dB): the game doesn't play it
        if gain < 1e-4 {
            continue;
        }
        let (attack, _) = sfx.sounds.attack_release(e.sound);
        let Sfx { sounds, banks, seed, muted, volume, .. } = &mut *sfx;
        let mut pick = || {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 17;
            *seed ^= *seed << 5;
            (*seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let layers = sounds.layers(e.sound, &mut pick);
        if std::env::var("LIBERTY_SOUND_DEBUG").is_ok() {
            info!("sound {} ({} layers){}", sounds.sounds.get(&e.sound).map_or("?", |s| s.name.as_str()), layers.len(), if *muted { ", muted" } else { "" });
        }
        if *muted {
            continue;
        }
        let Some(mut mixed) = rage::audio::mix(&layers, sounds, banks) else { continue };
        if pan != 0.0 {
            let (l, r) = (1.0 - 0.7 * pan.max(0.0), 1.0 + 0.7 * pan.min(0.0));
            for f in mixed.chunks_exact_mut(2) {
                f[0] = (f[0] as f32 * l) as i16;
                f[1] = (f[1] as f32 * r) as i16;
            }
        }
        // its attack: a linear rise over the first `attack` s (the mix is 44.1 kHz stereo)
        let rise = (attack * 44_100.0) as usize;
        for (i, f) in mixed.chunks_exact_mut(2).take(rise).enumerate() {
            let k = i as f32 / rise as f32;
            f[0] = (f[0] as f32 * k) as i16;
            f[1] = (f[1] as f32 * k) as i16;
        }
        let source = sources.add(AudioSource { bytes: rage::audio::wav_stereo(&mixed).into() });
        commands.spawn((AudioPlayer::new(source), PlaybackSettings::DESPAWN.with_volume(Volume::Linear(gain * MASTER * *volume))));
    }
}

// ---------------------------------------------------------------------------------------------
// The engine of the car being driven. The samples are the game's (each car's audio object in
// game.dat16 names its engine and exhaust loops at low and high revs and at idle). How they are
// driven is OUR stand-in: IV's rev model and its volume / pitch curves (curves.dat12) aren't
// decoded. Here: an automatic gearbox over handling.dat's gear count and top speed gives revs
// 0..1; idle fades out as revs rise, the low loop carries the middle, the high loop comes in with
// revs and throttle, and both loops rise in pitch with revs.
// ---------------------------------------------------------------------------------------------

/// The three looping layers of the running engine.
#[derive(Component)]
pub struct EngineLoop(u8);

#[derive(Resource, Default)]
pub struct Engine {
    car: Option<Entity>,
    /// 0..1 since the loops started (they rise instead of starting at full level).
    fade: f32,
    gear: u32,
    revs: f32,
    log: f32,
}

impl Sfx {
    /// A car model's sound whose name ends with `suffix` (e.g. `_ENGINE_LOW`).
    fn car_sound(&self, model: &str, suffix: &str) -> Option<u32> {
        let object = self.car_objects.get(&model.to_ascii_lowercase()).cloned().unwrap_or_else(|| model.to_ascii_uppercase());
        self.game.get(&object).or_else(|| self.game.get(&model.to_ascii_uppercase()))?.iter().copied().find(|h| self.sounds.sounds.get(h).is_some_and(|s| s.name.ends_with(suffix)))
    }

    /// Several sounds mixed into one buffer.
    fn mixed(&mut self, ids: &[u32]) -> Option<Vec<i16>> {
        let Sfx { sounds, banks, seed, .. } = self;
        let mut pick = || {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 17;
            *seed ^= *seed << 5;
            (*seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let layers: Vec<_> = ids.iter().flat_map(|&id| sounds.layers(id, &mut pick)).collect();
        rage::audio::mix(&layers, sounds, banks)
    }
}

pub fn engine(
    mut commands: Commands,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut sfx: ResMut<Sfx>,
    mut engine: ResMut<Engine>,
    fly: Query<&Fly>,
    cars: Query<&Car>,
    mut loops: Query<(Entity, &EngineLoop, Option<&mut AudioSink>), Without<FadeOut>>,
    mut sources: ResMut<Assets<AudioSource>>,
    mut play: EventWriter<PlaySfx>,
) {
    let _slow = crate::SlowGuard::new("sound::engine");
    let dt = time.delta_secs().min(0.05);
    let driving = fly.single().ok().and_then(|f| f.driving);
    let car = driving.and_then(|e| cars.get(e).ok());
    if engine.car != driving.filter(|_| car.is_some()) {
        // got in, got out or changed car: stop the old loops, start this car's
        for (e, _, sink) in &loops {
            commands.entity(e).insert(FadeOut::new(sink.map_or(0.0, |s| s.volume().to_linear()), LOOP_FADE));
        }
        engine.car = driving.filter(|_| car.is_some());
        engine.fade = 0.0;
        engine.gear = 1;
        engine.revs = 0.0;
        if let Some(car) = car {
            let sets: [(u8, [&str; 2]); 3] = [(0, ["_ENG_IDLE_LOOP", "_EX_IDLE_LOOP"]), (1, ["_ENGINE_LOW", "_EXHAUST_LOW"]), (2, ["_ENGINE_HIGH", "_EXHAUST_HIGH"])];
            let mut found = 0;
            for (layer, suffixes) in sets {
                let ids: Vec<u32> = suffixes.iter().filter_map(|s| sfx.car_sound(&car.model, s)).collect();
                found += ids.len();
                if sfx.muted {
                    continue;
                }
                if let Some(m) = sfx.mixed(&ids) {
                    let source = sources.add(AudioSource { bytes: rage::audio::wav_stereo(&m).into() });
                    commands.spawn((AudioPlayer::new(source), PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)), EngineLoop(layer)));
                }
            }
            if std::env::var("LIBERTY_SOUND_DEBUG").is_ok() {
                info!("engine: {} ({} gears, top {:.0} km/h): {found} of 6 loops found", car.model, car.gears, car.top_speed * 3.6);
            }
            if let Some(start) = sfx.car_sound(&car.model, "_START") {
                play.write(PlaySfx { sound: start, volume: 0.7, pan: 0.0, at: None });
            }
        }
    }
    let Some(car) = car else { return };
    // H: the car's own horn
    if keys.just_pressed(KeyCode::KeyH) {
        if let Some(horn) = sfx.car_sound(&car.model, "HORN") {
            play.write(PlaySfx { sound: horn, volume: 0.8, pan: 0.0, at: None });
        }
    }

    // --- revs from speed through an automatic gearbox (stand-in)
    let speed = car.speed_kmh.abs() / 3.6;
    let gears = car.gears.max(1);
    let gear_top = |g: u32| car.top_speed * (g as f32 / gears as f32).powf(0.85);
    let mut gear = engine.gear.clamp(1, gears);
    if car.reversing {
        gear = 1;
    }
    let in_gear = |g: u32| (speed / gear_top(g)).min(1.2);
    if in_gear(gear) > 0.92 && gear < gears {
        gear += 1;
    } else if gear > 1 && in_gear(gear - 1) < 0.6 {
        gear -= 1;
    }
    // revs: road speed in this gear, lifted a little by the throttle (the engine pulling)
    let target = (0.12 + 0.88 * in_gear(gear).min(1.0)) * (0.82 + 0.18 * car.throttle) + 0.1 * car.throttle * (1.0 - in_gear(gear)).max(0.0);
    engine.gear = gear;
    engine.revs += (target - engine.revs) * (dt * if target > engine.revs { 5.0 } else { 3.0 }).min(1.0);
    let r = engine.revs.clamp(0.0, 1.0);

    let smooth = |a: f32, b: f32, x: f32| {
        let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let load = 0.55 + 0.45 * car.throttle;
    let levels = [
        // idle: only near tick-over
        (1.0 - smooth(0.12, 0.3, r), 1.0 + (r - 0.12).max(0.0) * 1.5),
        // low loop: the body of the sound, a little quieter at the top
        (smooth(0.1, 0.28, r) * (1.0 - 0.45 * smooth(0.55, 1.0, r)) * load, 0.6 + 0.85 * r),
        // high loop: comes in with revs
        (smooth(0.35, 0.9, r) * load, 0.55 + 0.6 * r),
    ];
    engine.fade = (engine.fade + dt / LOOP_FADE).min(1.0);
    let master = MASTER * sfx.volume * 0.55 * engine.fade;
    for (_, l, sink) in &mut loops {
        if let Some(mut sink) = sink {
            let (v, pitch) = levels[l.0 as usize];
            sink.set_volume(Volume::Linear(v * master));
            sink.set_speed(pitch);
        }
    }
    engine.log += dt;
    if engine.log > 0.5 && std::env::var("LIBERTY_SOUND_DEBUG").is_ok() {
        engine.log = 0.0;
        info!("engine: {:.0} km/h gear {gear} revs {r:.2} throttle {:.0}: idle {:.2} low {:.2}@{:.2} high {:.2}@{:.2}", car.speed_kmh, car.throttle, levels[0].0, levels[1].0, levels[1].1, levels[2].0, levels[2].1);
    }
}

// ---------------------------------------------------------------------------------------------
// City ambience: the game's ambient zones and rules (game.dat16). Where the camera is picks the
// zones; each of their rules plays its sound now and then, in its hours, as if from its distance.
// Only the one-shot rule sounds: the continuous city bed is in the streamed (multichannel) banks,
// which aren't read yet.
// ---------------------------------------------------------------------------------------------

#[derive(Resource)]
pub struct Ambience {
    zones: Vec<rage::audio::AmbientZone>,
    rules: HashMap<u32, rage::audio::AmbientRule>,
    /// When each rule plays next (seconds of app time).
    next: HashMap<u32, f32>,
    seed: u32,
    zone_log: String,
}

impl Ambience {
    pub fn load(root: &Path) -> Result<Ambience> {
        let (zones, rules) = rage::audio::ambience(root)?;
        Ok(Ambience { zones, rules, next: HashMap::new(), seed: 0x9E37_79B9, zone_log: String::new() })
    }
}

pub fn ambience(
    time: Res<Time>,
    mut amb: ResMut<Ambience>,
    cam: Query<&GlobalTransform, With<Camera3d>>,
    tc: Option<Res<crate::timecyc::TimeCycle>>,
    sfx: Res<Sfx>,
    mut play: EventWriter<PlaySfx>,
) {
    let _slow = crate::SlowGuard::new("sound::ambience");
    let Ok(cam) = cam.single() else { return };
    let (x, y) = (cam.translation().x, -cam.translation().z);
    let hour = tc.map_or(12.0, |t| t.hour);
    let now = time.elapsed_secs();
    let Ambience { zones, rules, next, seed, zone_log } = &mut *amb;
    let mut rand = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        (*seed >> 8) as f32 / (1u32 << 24) as f32
    };
    let here: Vec<&rage::audio::AmbientZone> = zones.iter().filter(|z| x >= z.min[0] && x <= z.max[0] && y >= z.min[1] && y <= z.max[1]).collect();
    if std::env::var("LIBERTY_SOUND_DEBUG").is_ok() {
        let names = here.iter().map(|z| z.name.as_str()).collect::<Vec<_>>().join(", ");
        if names != *zone_log {
            info!("ambience: at {x:.0}, {y:.0} in [{names}]");
            *zone_log = names;
        }
    }
    let mut active: Vec<u32> = here.iter().flat_map(|z| z.rules.iter().copied()).collect();
    active.sort_unstable();
    active.dedup();
    // rules of zones we've left start afresh when we come back
    next.retain(|r, _| active.contains(r));
    for r in active {
        let Some(rule) = rules.get(&r) else { continue };
        let in_hours = if rule.start_hour <= rule.end_hour { hour >= rule.start_hour as f32 && hour < rule.end_hour as f32 } else { hour >= rule.start_hour as f32 || hour < rule.end_hour as f32 };
        if rule.weight <= 0.0 || !in_hours {
            continue;
        }
        let interval = rule.interval.max(1.0);
        let at = *next.entry(r).or_insert_with(|| now + rand() * interval);
        if now >= at {
            next.insert(r, now + interval * (0.6 + 0.8 * rand()));
            // as if from somewhere in its distance band, with the game's rolloff for that sound (its
            // category's scale). Until 11 Oct a stand-in law (0.8 x 12 m / distance; before 8 Oct
            // 0.9 x 60 m / distance, which played the 100-180 m sirens like his own car's).
            let dist = rule.min_dist + rand() * (rule.max_dist - rule.min_dist).max(0.0);
            let volume = sfx.distance_gain(rule.sound, dist);
            if std::env::var("LIBERTY_SOUND_DEBUG").is_ok() {
                info!("ambience: {} at {dist:.0} m, volume {volume:.2}", rule.name);
            }
            play.write(PlaySfx { sound: rule.sound, volume, pan: rand() * 2.0 - 1.0, at: None });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Traffic: the engines of the cars around you. Each of the nearest few traffic cars plays its own
// model's low engine + exhaust loops (the same samples the driven car uses), placed on the car.
// Loudness with distance is the game's (DEFAULT_ROLLOFF over CARS_ENGINES' rolloff scale), pan from
// Bevy's spatial listener. STAND-INS: how many cars are heard (5 within 150 m), loudness and pitch
// against speed, the 0.1 s smoothing of level changes and LOOP_FADE in/out (the engine loops set
// no attack or release of their own). No tyre noise yet.
// ---------------------------------------------------------------------------------------------

/// A loop being stopped: its volume falls from `from` to 0 over `total` s, then it's despawned.
#[derive(Component)]
pub struct FadeOut {
    from: f32,
    left: f32,
    total: f32,
}

impl FadeOut {
    fn new(from: f32, secs: f32) -> FadeOut {
        FadeOut { from, left: secs, total: secs.max(0.01) }
    }
}

/// Fade-in / fade-out of loops whose sounds set no attack or release (STAND-IN).
const LOOP_FADE: f32 = 0.3;

pub fn fade_out(
    mut commands: Commands,
    time: Res<Time>,
    mut fading: Query<(Entity, &mut FadeOut, Option<&mut AudioSink>, Option<&mut bevy::audio::SpatialAudioSink>)>,
) {
    for (e, mut f, sink, spatial) in &mut fading {
        f.left -= time.delta_secs();
        let v = Volume::Linear(f.from * (f.left / f.total).max(0.0));
        if let Some(mut s) = sink {
            s.set_volume(v);
        }
        if let Some(mut s) = spatial {
            s.set_volume(v);
        }
        if f.left <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

/// A traffic car's engine loop (a child of the car, so it sits where the car is).
#[derive(Component)]
pub struct TrafficLoop {
    /// The sound its distance gain is worked out for (the model's _ENGINE_LOW).
    sound: u32,
    level: f32,
}

#[derive(Resource, Default)]
pub struct TrafficSound {
    /// One mixed loop per car model, with the sound that sets its rolloff (None: no engine sounds).
    cache: HashMap<String, Option<(Handle<AudioSource>, u32)>>,
    heard: HashMap<Entity, Entity>,
    timer: f32,
    log: f32,
}

const TRAFFIC_HEARD: usize = 5;
const TRAFFIC_RANGE: f32 = 150.0;

pub fn traffic_engines(
    mut commands: Commands,
    time: Res<Time>,
    mut sfx: ResMut<Sfx>,
    mut state: ResMut<TrafficSound>,
    cam: Query<&GlobalTransform, With<Camera3d>>,
    cars: Query<(Entity, &Transform, &Car, &crate::traffic::Traffic)>,
    mut sinks: Query<(&ChildOf, &mut TrafficLoop, &mut bevy::audio::SpatialAudioSink), Without<FadeOut>>,
    mut sources: ResMut<Assets<AudioSource>>,
) {
    let _slow = crate::SlowGuard::new("sound::traffic_engines");
    let dt = time.delta_secs();
    state.timer -= dt;
    state.log += dt;
    let Ok(cam) = cam.single() else { return };
    let ear = cam.translation();
    if state.timer <= 0.0 {
        state.timer = 0.3;
        let mut near: Vec<(f32, Entity, &str)> =
            cars.iter().map(|(e, t, car, _)| (t.translation.distance(ear), e, car.model.as_str())).filter(|c| c.0 < TRAFFIC_RANGE).collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.truncate(TRAFFIC_HEARD);
        // the ones that fell out of the nearest few fade out (a removed car takes its loop with it)
        let keep: Vec<Entity> = near.iter().map(|n| n.1).collect();
        let gone: Vec<Entity> = state.heard.keys().copied().filter(|c| !keep.contains(c)).collect();
        for car in gone {
            if let Some(l) = state.heard.remove(&car) {
                if let Ok((_, tl, _)) = sinks.get(l) {
                    commands.entity(l).insert(FadeOut::new(tl.level, LOOP_FADE));
                }
            }
        }
        for (_, car, model) in near {
            if state.heard.contains_key(&car) {
                continue;
            }
            let source = match state.cache.get(model) {
                Some(s) => s.clone(),
                None => {
                    let ids: Vec<u32> = ["_ENGINE_LOW", "_EXHAUST_LOW"].iter().filter_map(|s| sfx.car_sound(model, s)).collect();
                    let s = sfx.mixed(&ids).map(|m| (sources.add(AudioSource { bytes: rage::audio::wav_stereo(&m).into() }), ids[0]));
                    state.cache.insert(model.to_string(), s.clone());
                    s
                }
            };
            let Some((source, sound)) = source else { continue };
            if sfx.muted {
                state.heard.insert(car, Entity::PLACEHOLDER);
                continue;
            }
            // scaled so rodio's own 1/d^2 never bites inside 400 m: the game's rolloff sets the level
            let settings = PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)).with_spatial(true).with_spatial_scale(bevy::audio::SpatialScale::new(1.0 / 400.0));
            let l = commands.spawn((AudioPlayer::new(source), settings, Transform::default(), TrafficLoop { sound, level: 0.0 })).id();
            commands.entity(car).add_child(l);
            state.heard.insert(car, l);
        }
    }
    let master = MASTER * sfx.volume * 0.4;
    let mut loudest = (0.0f32, 0.0f32);
    for (parent, mut tl, mut sink) in &mut sinks {
        let Ok((_, t, _, tr)) = cars.get(parent.parent()) else { continue };
        let s = tr.speed();
        let d = t.translation.distance(ear);
        let target = master * (0.45 + 0.55 * (s / 12.0).min(1.0)) * sfx.distance_gain(tl.sound, d);
        tl.level += (target - tl.level) * (dt / 0.1).min(1.0);
        if tl.level > loudest.0 {
            loudest = (tl.level, d);
        }
        sink.set_volume(Volume::Linear(tl.level));
        sink.set_speed(0.7 + 0.5 * (s / 14.0).min(1.0));
    }
    if state.log > 1.0 && std::env::var("LIBERTY_SOUND_DEBUG").is_ok() {
        state.log = 0.0;
        info!(
            "traffic engines: {} cars heard, loudest level {:.4} at {:.0} m, {} models cached ({} without engine sounds)",
            state.heard.len(),
            loudest.0,
            loudest.1,
            state.cache.len(),
            state.cache.values().filter(|s| s.is_none()).count()
        );
    }
}
