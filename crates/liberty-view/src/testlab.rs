//! The test lab: one shared set of test stations, so streams stop building their own test setups
//! and re-recording the same "before" (plans/TESTLAB.md; runner `re/lab.sh`).
//!
//!   liberty-view --testlab <station>[/<take>] --city city --record <dir> --record-secs <s>
//!   liberty-view --testlab-list            one line per take: station take secs sheet-first sheet-every
//!
//! A station is a fixed spot with its own scripted input and camera: most stand on the test pad
//! (testpad.rs, with their props), two are real places in the city. `--testlab` sets the station's
//! environment (scripted presses, camera, the pad) before anything reads it; a variable the caller
//! already set wins, so `re/lab.sh run <station> ENV=VAL` varies one thing. Setting `LIBERTY_CAM_AT`
//! runs a pad station at that place in the city instead. Every lab run is hidden, silent and on the
//! fixed clock.
//!
//! While it records, the probe here measures what the station is about (where Niko went and how
//! fast, his jumps, a ragdoll's tilt / COM / steps / sinking, the target ped, the car) and writes
//! `<record dir>.json` when the run ends: flat keys, numbers (0 / 1 for yes / no) or short words.
//! `re/lab.sh` merges the takes into the station's numbers.json and compares it with the baseline.

use bevy::prelude::*;

use crate::collision;
use crate::ped::Player;
use crate::peds::Npc;
use crate::ragdoll::{RagdollDef, RagdollSim};
use crate::stream::City;
use crate::testpad::{self, AT_CAR, AT_CARHIT, AT_GROUND_KERB, AT_JUMP, AT_KERB, AT_MOVE, AT_SHOT, AT_SHOVE, AT_SLOPE, AT_WATER};
use crate::vehicle::Car;
use crate::Fly;

type Env = &'static [(&'static str, &'static str)];

enum Place {
    /// A spot on the test pad (metres east / north of its centre).
    Pad([f32; 2]),
    /// A real place: LIBERTY_CAM_AT (a model name or @x,y,z).
    City(&'static str),
}

struct Take {
    name: &'static str,
    /// Overrides the station's spot (same scripted input elsewhere).
    at: Option<[f32; 2]>,
    env: Env,
}

struct Station {
    name: &'static str,
    place: Place,
    secs: f32,
    /// The picture sheet: first frame and every n-th (frames are 1/30 s apart).
    sheet: (u32, u32),
    env: Env,
    /// Empty: one take, named after the station.
    takes: &'static [Take],
}

// Scripted presses sit off the 1/60 s frame grid (0.81, not 0.8): ped.rs's press check can skip a
// press that lands exactly on a frame (float rounding against its double-press margin).
const SHOOT: Env = &[
    ("LIBERTY_TARGET_PED", "2.6"),
    ("LIBERTY_WEAPON", "pistol"),
    ("LIBERTY_AIM", "1"),
    ("LIBERTY_FIRE_AT", "2"),
    ("LIBERTY_FIRE_FOR", "0.05"),
    ("LIBERTY_PED_CAMYAW", "90"),
    ("LIBERTY_RAG_DEBUG", "1"),
];
/// A body shot: the camera's ray (and so the bullet) meets the target at the hips. (At -0.05 on the
/// bare pad the target ped never spawns: peds.rs waits for the camera to stand still, and it doesn't.)
const BODY: Env = &[("LIBERTY_PED_CAMPITCH", "-0.1")];

/// The stations. Who uses them: move / jump (S5), shove / shot / scorecard (S1, S2), carhit (S3, S1),
/// car / water / ground (S3), kerb (the skate pack, run with its own exe), tennis / starjunction
/// (the city as it plays).
const STATIONS: &[Station] = &[
    // stand, W start straight / 180 / 90, walk, run, 180 turn at speed, sprint, stops
    Station {
        name: "move",
        place: Place::Pad(AT_MOVE),
        secs: 20.0,
        sheet: (0, 37),
        env: &[("LIBERTY_PED_SCRIPT", "idle:1,walk:2.5,idle:1.5,run:2.5,back:2,idle:2,sprint:2.5,idle:2,right:2,idle:2")],
        takes: &[],
    },
    // a standing jump at 0.8 s, a running one at 5.5 s (well after the 90-degree start clip), across the view
    Station { name: "jump", place: Place::Pad(AT_JUMP), secs: 7.5, sheet: (0, 14), env: &[("LIBERTY_PED_SCRIPT", "idle:2,right:5.5"), ("LIBERTY_JUMP_AT", "0.81,5.51")], takes: &[] },
    // Niko shoved from standing at 1 s (J), seen side-on, on IV's balancer
    Station {
        name: "shove",
        place: Place::Pad(AT_SHOVE),
        secs: 9.0,
        sheet: (25, 10),
        env: &[("LIBERTY_SHOVE_AT", "1.01"), ("LIBERTY_NM_BALANCE", "1"), ("LIBERTY_PED_CAMYAW", "90"), ("LIBERTY_RAG_DEBUG", "1")],
        takes: &[Take { name: "1.0", at: None, env: &[("LIBERTY_SHOVE_SPEED", "1.0")] }, Take { name: "2.0", at: None, env: &[("LIBERTY_SHOVE_SPEED", "2.0")] }],
    },
    // a ped 2.6 m away shot once with the pistol at 2 s: in the shins (as re/shot_test.sh did) and in the hips
    Station {
        name: "shot",
        place: Place::Pad(AT_SHOT),
        secs: 8.0,
        sheet: (50, 6),
        env: &[],
        takes: &[Take { name: "leg", at: None, env: &[] }, Take { name: "body", at: None, env: BODY }],
    },
    // a car driven at Niko (IV's recorded hit was 7.5 m/s), seen side-on
    Station {
        name: "carhit",
        place: Place::Pad(AT_CARHIT),
        secs: 8.0,
        sheet: (30, 10),
        env: &[("LIBERTY_RAM_TEST", "1"), ("LIBERTY_PED_CAMYAW", "90"), ("LIBERTY_RAG_DEBUG", "1")],
        takes: &[Take { name: "7.5", at: None, env: &[("LIBERTY_RAM_SPEED", "7.5")] }, Take { name: "11", at: None, env: &[("LIBERTY_RAM_SPEED", "11")] }],
    },
    // walk to the parked car and get in, drive west over four bumps and a kerb, brake, get out
    Station {
        name: "car",
        place: Place::Pad(AT_CAR),
        secs: 18.0,
        sheet: (0, 34),
        env: &[("LIBERTY_PARK", "1"), ("LIBERTY_CAR", "infernus"), ("LIBERTY_CAR_IN", "0.51"), ("LIBERTY_THROTTLE_AT", "6,10"), ("LIBERTY_BRAKE_AT", "10"), ("LIBERTY_CAR_OUT", "14.01")],
        takes: &[],
    },
    // a ped on the edge of a 2 m dock, shot in the hips, into 1.7 m of water
    Station { name: "water", place: Place::Pad(AT_WATER), secs: 9.0, sheet: (50, 8), env: &[("LIBERTY_PED_CAMPITCH", "-0.05")], takes: &[] },
    // a ped shot in the hips on a 15 degree slope, and in the road falling across a kerb (half in the ground?)
    Station {
        name: "ground",
        place: Place::Pad(AT_SLOPE),
        secs: 9.0,
        sheet: (50, 8),
        env: &[("LIBERTY_PED_CAMPITCH", "-0.05")],
        takes: &[Take { name: "slope", at: Some(AT_SLOPE), env: &[] }, Take { name: "kerb", at: Some(AT_GROUND_KERB), env: &[] }],
    },
    // a jog over a 15 cm kerb, past a rail, up a 1 m ramp and off the ledge (the skate pack's kerb, rail, ramp)
    Station { name: "kerb", place: Place::Pad(AT_KERB), secs: 7.0, sheet: (0, 13), env: &[("LIBERTY_PED_SCRIPT", "idle:1,run:6")], takes: &[] },
    // the city as it plays: on foot at the tennis courts, and at Star Junction
    Station { name: "tennis", place: Place::City("am_fm2_tennis"), secs: 6.0, sheet: (0, 11), env: &[("LIBERTY_PED_SCRIPT", "idle:1,walk:5")], takes: &[] },
    Station { name: "starjunction", place: Place::City("@-206,379,15"), secs: 6.0, sheet: (0, 11), env: &[("LIBERTY_PED_SCRIPT", "idle:1,walk:5")], takes: &[] },
];

/// The water / ground stations shoot too.
fn shoots(station: &str) -> bool {
    matches!(station, "shot" | "water" | "ground")
}

fn takes(s: &Station) -> Vec<&'static str> {
    if s.takes.is_empty() { vec![s.name] } else { s.takes.iter().map(|t| t.name).collect() }
}

/// `--testlab-list`: one line per take (for re/lab.sh).
pub fn list() {
    for s in STATIONS {
        for t in takes(s) {
            println!("{} {} {} {} {}", s.name, t, s.secs, s.sheet.0, s.sheet.1);
        }
    }
}

fn set(k: &str, v: &str) {
    if std::env::var_os(k).is_none() {
        // SAFETY: called from main before the app (or any other thread) starts
        unsafe { std::env::set_var(k, v) };
    }
}

/// `--testlab <station>[/<take>]`: set the station's environment (what the caller set wins).
pub fn apply(spec: &str) -> anyhow::Result<Lab> {
    let (name, take) = spec.split_once('/').unwrap_or((spec, ""));
    let Some(s) = STATIONS.iter().find(|s| s.name == name) else {
        anyhow::bail!("no test lab station {name:?}; stations: {}", STATIONS.iter().map(|s| s.name).collect::<Vec<_>>().join(" "));
    };
    let t = if s.takes.is_empty() {
        None
    } else {
        let t = if take.is_empty() { &s.takes[0] } else { s.takes.iter().find(|t| t.name == take).ok_or_else(|| anyhow::anyhow!("station {name} has no take {take:?}; takes: {}", takes(s).join(" ")))? };
        Some(t)
    };
    // hidden, silent, unsteerable, on the fixed clock: always; on foot unless the caller says
    // LIBERTY_WALK=0 (a free camera over a city station, e.g. a top-down look at peds)
    for (k, v) in [("LIBERTY_HIDDEN", "1"), ("LIBERTY_NO_INPUT", "1"), ("LIBERTY_SOUND_VOLUME", "0"), ("LIBERTY_FIXED_DT", "1"), ("LIBERTY_WALK", "1")] {
        if k == "LIBERTY_WALK" && std::env::var_os(k).is_some() {
            continue;
        }
        // SAFETY: as in `set`
        unsafe { std::env::set_var(k, v) };
    }
    for (k, v) in t.map_or(&[][..], |t| t.env).iter().chain(s.env) {
        set(k, v);
    }
    if shoots(name) {
        for (k, v) in SHOOT {
            set(k, v);
        }
    }
    match s.place {
        // the caller's LIBERTY_CAM_AT takes a pad station into the city
        Place::Pad(at) if std::env::var_os("LIBERTY_CAM_AT").is_none() => {
            let at = t.and_then(|t| t.at).unwrap_or(at);
            set("LIBERTY_TESTPAD", "1");
            set("LIBERTY_LAB_AT", &format!("{},{}", at[0], at[1]));
            set("LIBERTY_CAM_YAW", "0");
            set("LIBERTY_TRAFFIC", "0");
        }
        Place::Pad(_) => {}
        Place::City(at) => set("LIBERTY_CAM_AT", at),
    }
    Ok(Lab { station: name.to_string(), take: t.map_or(name, |t| t.name).to_string(), ..default() })
}

/// One running ragdoll's measures, from when it starts until it hands back to animation.
#[derive(Default)]
struct RagStats {
    started: Option<f32>,
    frames: u32,
    secs: f32,
    modes: Vec<String>,
    max_tilt: f32,
    min_com: Option<f32>,
    end_com: f32,
    start_com: Option<Vec3>,
    thrown: f32,
    steps: u32,
    anchor: [Option<Vec2>; 2],
    tipped: bool,
    sunk: Option<f32>,
    end_sunk: f32,
    recovered: bool,
    ended: bool,
    under_water_secs: f32,
    min_vs_water: Option<f32>,
}

impl RagStats {
    fn feed(&mut self, t: f32, dt: f32, sim: &RagdollSim, def: &RagdollDef, city: &City) {
        let chunks = city.collision.values();
        let (com, tilt, feet, parts) = sim.lab_probe(def);
        self.started.get_or_insert(t);
        self.frames += 1;
        self.secs += dt;
        let mode = format!("{:?}", sim.mode);
        let mode = mode.split([' ', '{']).next().unwrap_or("").to_string();
        if self.modes.last() != Some(&mode) {
            self.modes.push(mode);
        }
        self.max_tilt = self.max_tilt.max(tilt);
        let g = collision::ground_height(chunks.clone(), com.x, com.z, com.y).unwrap_or(com.y - 1.0);
        self.end_com = com.y - g;
        self.min_com = Some(self.min_com.map_or(self.end_com, |m| m.min(self.end_com)));
        let s0 = *self.start_com.get_or_insert(com);
        self.thrown = self.thrown.max((com - s0).with_y(0.0).length());
        // steps: a foot 0.15 m from where it last stood, until he tips past 45 degrees (as --closedloop)
        self.tipped |= tilt > 45.0;
        if !self.tipped {
            for (a, f) in self.anchor.iter_mut().zip(feet) {
                match a {
                    None => *a = Some(f.xz()),
                    Some(p) if p.distance(f.xz()) > 0.15 => {
                        self.steps += 1;
                        *a = Some(f.xz());
                    }
                    _ => {}
                }
            }
        }
        // sinking: how far the lowest part's origin is under the ground right above / below it
        // (negative = a part is in the ground)
        self.end_sunk = parts.iter().map(|p| p.y - collision::ground_height(chunks.clone(), p.x, p.z, p.y + 1.0).unwrap_or(p.y - 1.0)).fold(f32::MAX, f32::min);
        self.sunk = Some(self.sunk.map_or(self.end_sunk, |s| s.min(self.end_sunk)));
        self.recovered |= sim.recovered;
        if let Some(level) = testpad::lab_water_level(com) {
            let d = com.y - level;
            self.min_vs_water = Some(self.min_vs_water.map_or(d, |m| m.min(d)));
            if d < 0.0 {
                self.under_water_secs += dt;
            }
        }
    }

    fn fell(&self) -> bool {
        self.min_com.is_some_and(|c| c < 0.5) || self.max_tilt > 60.0
    }

    fn write(&self, out: &mut Vec<(String, String)>, who: &str) {
        let Some(t0) = self.started else {
            out.push((format!("{who}.ragdoll"), "0".into()));
            return;
        };
        let mut put = |k: &str, v: String| out.push((format!("{who}.{k}"), v));
        put("ragdoll", "1".into());
        put("rag_at", f(t0));
        put("rag_secs", f(self.secs));
        put("modes", format!("\"{}\"", self.modes.join(">")));
        put("fell", (self.fell() as u8).to_string());
        put("back_to_anim", ((self.ended && !self.fell()) as u8).to_string());
        put("recovered", (self.recovered as u8).to_string());
        put("max_tilt", f(self.max_tilt));
        put("min_com", f(self.min_com.unwrap_or(0.0)));
        put("end_com", f(self.end_com));
        put("steps", self.steps.to_string());
        put("thrown", f(self.thrown));
        put("sunk", f(self.sunk.unwrap_or(0.0)));
        put("end_sunk", f(self.end_sunk));
        if let Some(w) = self.min_vs_water {
            put("min_com_vs_water", f(w));
            put("under_water_secs", f(self.under_water_secs));
        }
    }
}

fn f(v: f32) -> String {
    if v.is_finite() { format!("{v:.3}") } else { "null".into() }
}

#[derive(Default, Clone, Copy)]
struct Sample {
    t: f32,
    pos: Vec3,
    /// Height over the ground under him.
    h: f32,
    on_foot: bool,
}

#[derive(Default, Clone, Copy)]
struct CarSample {
    t: f32,
    pos: Vec3,
    rot: Quat,
    kmh: f32,
}

/// What the probe has seen so far in this run.
#[derive(Resource, Default)]
pub struct Lab {
    station: String,
    take: String,
    niko: Vec<Sample>,
    niko_rag: RagStats,
    had_rag: bool,
    ped: RagStats,
    ped_seen: bool,
    ped_had_rag: bool,
    ped_back_up: bool,
    car: Vec<CarSample>,
    /// Recording time Niko was last seen driving, and on foot again after that.
    on_foot_after_car: Option<f32>,
    wall_ms: Vec<f32>,
    written: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn probe(
    time: Res<Time>,
    mut last: Local<Option<std::time::Instant>>,
    started: Option<Res<crate::RecordStart>>,
    city: Res<City>,
    mut lab: ResMut<Lab>,
    player: Query<(&Transform, &Player)>,
    fly: Query<&Fly>,
    cars: Query<(&Transform, &Car)>,
    npcs: Query<(&Transform, &Npc)>,
) {
    let Some(s) = started else { return };
    let t = time.elapsed_secs() - s.0;
    let dt = time.delta_secs();
    let lab = &mut *lab;
    let now = std::time::Instant::now();
    if let Some(l) = last.replace(now) {
        lab.wall_ms.push(now.duration_since(l).as_secs_f32() * 1000.0); // the PC's clock: Time<Real> follows the fixed one
    }
    let driving = fly.single().ok().and_then(|f| f.driving);
    if let Ok((tf, p)) = player.single() {
        let (rag, _clip, car) = p.lab();
        let pos = tf.translation;
        let g = collision::ground_height(city.collision.values(), pos.x, pos.z, pos.y + 0.5).unwrap_or(pos.y);
        let on_foot = p.active && driving.is_none() && car == "none";
        lab.niko.push(Sample { t, pos, h: pos.y - g, on_foot });
        if let Some((sim, def)) = rag {
            lab.niko_rag.feed(t, dt, sim, def, &city);
            lab.had_rag = true;
        } else if lab.had_rag {
            lab.niko_rag.ended = true;
        }
        if on_foot && !lab.car.is_empty() && lab.on_foot_after_car.is_none() {
            lab.on_foot_after_car = Some(t);
        }
    }
    if let Some((tf, c)) = driving.and_then(|e| cars.get(e).ok()) {
        lab.car.push(CarSample { t, pos: tf.translation, rot: tf.rotation, kmh: c.speed_kmh });
    }
    if std::env::var_os("LIBERTY_TARGET_PED").is_some() {
        // the target: the ped nearest the station (the pad has only him)
        let here = player.single().map_or(Vec3::ZERO, |(t, _)| t.translation);
        if let Some((_, npc)) = npcs.iter().min_by(|a, b| a.0.translation.distance(here).total_cmp(&b.0.translation.distance(here))) {
            lab.ped_seen = true;
            if let Some((sim, def)) = npc.lab_rag() {
                lab.ped.feed(t, dt, sim, def, &city);
                lab.ped_had_rag = true;
            } else if lab.ped_had_rag {
                lab.ped.ended = true;
                lab.ped_back_up |= !npc.is_down();
            }
        }
    }
}

/// The script's steps (ped.rs's LIBERTY_PED_SCRIPT): (mode, from, to) in recording seconds.
fn script() -> Vec<(String, f32, f32)> {
    let mut at = 0.0;
    std::env::var("LIBERTY_PED_SCRIPT")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|step| {
            let (mode, secs) = step.split_once(':').unwrap_or((step, "1"));
            let secs: f32 = secs.parse().unwrap_or(1.0);
            at += secs;
            (mode.trim().to_string(), at - secs, at)
        })
        .collect()
}

fn env_times(var: &str) -> Vec<f32> {
    std::env::var(var).unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect()
}

impl Lab {
    fn numbers(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = vec![("station".into(), format!("\"{}\"", self.station)), ("take".into(), format!("\"{}\"", self.take))];
        let n = &self.niko;
        // speeds between samples (horizontal)
        let speed: Vec<f32> = (0..n.len()).map(|i| if i == 0 { 0.0 } else { (n[i].pos - n[i - 1].pos).with_y(0.0).length() / (n[i].t - n[i - 1].t).max(1e-4) }).collect();
        let on_foot: Vec<usize> = (0..n.len()).filter(|&i| n[i].on_foot).collect();
        if let (Some(&a), Some(&b)) = (on_foot.first(), on_foot.last()) {
            // walked: only frames on foot one after the other (not across a drive)
            out.push(("niko.path".into(), f(on_foot.windows(2).filter(|w| w[1] == w[0] + 1).map(|w| (n[w[1]].pos - n[w[0]].pos).with_y(0.0).length()).sum())));
            out.push(("niko.moved".into(), f((n[b].pos - n[a].pos).with_y(0.0).length())));
            out.push(("niko.max_speed".into(), f(on_foot.iter().map(|&i| speed[i]).fold(0.0, f32::max))));
            out.push(("niko.max_climb".into(), f(on_foot.iter().map(|&i| n[i].pos.y - n[a].pos.y).fold(f32::MIN, f32::max))));
        }
        // the script's steps: speeds, how fast they're reached, the stops
        let moving = |m: &str| m != "idle";
        for (k, (mode, a, b)) in script().iter().enumerate() {
            let idx: Vec<usize> = (0..n.len()).filter(|&i| n[i].t >= *a && n[i].t < *b && n[i].on_foot).collect();
            if idx.is_empty() {
                continue;
            }
            let key = format!("step{k:02}_{mode}");
            let sp: Vec<f32> = idx.iter().map(|&i| speed[i]).collect();
            let max = sp.iter().copied().fold(0.0, f32::max);
            out.push((format!("{key}.mean_speed"), f(sp.iter().sum::<f32>() / sp.len() as f32)));
            out.push((format!("{key}.max_speed"), f(max)));
            out.push((format!("{key}.end_speed"), f(*sp.last().unwrap())));
            out.push((format!("{key}.dist"), f((n[*idx.last().unwrap()].pos - n[idx[0]].pos).with_y(0.0).length())));
            if moving(mode) {
                // seconds to 90% of the step's top speed, and how long he stood still in it
                let t90 = idx.iter().find(|&&i| speed[i] >= 0.9 * max).map_or(f32::NAN, |&i| n[i].t - a);
                out.push((format!("{key}.t90"), f(t90)));
                out.push((format!("{key}.stuck_secs"), f(idx.iter().filter(|&&i| speed[i] < 0.5 && n[i].t - a > 0.5).count() as f32 / 60.0)));
            } else if k > 0 {
                // a stop: seconds until still, and how far he slid
                let still = idx.iter().find(|&&i| speed[i] < 0.1).map_or(f32::NAN, |&i| n[i].t - a);
                out.push((format!("{key}.t_still"), f(still)));
            }
        }
        // time off the ground (jumps, the ledge): rise, air time, distance and speed through the air
        let mut k = 0;
        let mut i = 0;
        while i < n.len() {
            if n[i].on_foot && n[i].h > 0.03 {
                let j0 = i;
                while i < n.len() && n[i].h > 0.03 {
                    i += 1;
                }
                let j1 = i.min(n.len() - 1);
                let air = n[j1].t - n[j0].t;
                if air >= 0.08 {
                    // a scripted jump is named after its press (so a missed one is a missing number,
                    // not a shift of the others); anything else (a ledge) by its order
                    let press = std::env::var("LIBERTY_JUMP_AT").unwrap_or_default().split(',').map(str::trim).filter(|p| p.parse::<f32>().is_ok_and(|p| p <= n[j0].t + 0.05 && n[j0].t - p < 1.0)).last().map(str::to_string);
                    let key = press.map_or_else(|| format!("air{k}"), |p| format!("jump_{p}"));
                    k += 1;
                    let d = (n[j1].pos - n[j0].pos).with_y(0.0).length();
                    out.push((format!("{key}.at"), f(n[j0].t)));
                    out.push((format!("{key}.rise"), f(n[j0..=j1].iter().map(|s| s.h).fold(0.0, f32::max))));
                    out.push((format!("{key}.secs"), f(air)));
                    out.push((format!("{key}.dist"), f(d)));
                    out.push((format!("{key}.speed"), f(d / air.max(1e-3))));
                }
            }
            i += 1;
        }
        if self.had_rag {
            self.niko_rag.write(&mut out, "niko");
        }
        if self.ped_seen {
            out.push(("ped.hit".into(), (self.ped_had_rag as u8).to_string()));
            self.ped.write(&mut out, "ped");
            out.push(("ped.back_up".into(), (self.ped_back_up as u8).to_string()));
        }
        // the car: getting in, the drive (bumps: the body's pitch / roll and vertical acceleration), the stop, getting out
        let c = &self.car;
        if !c.is_empty() {
            if let Some(&cin) = env_times("LIBERTY_CAR_IN").first() {
                out.push(("car.enter_secs".into(), f(c[0].t - cin)));
            }
            out.push(("car.max_kmh".into(), f(c.iter().map(|s| s.kmh).fold(0.0, f32::max))));
            out.push(("car.dist".into(), f(c.windows(2).map(|w| (w[1].pos - w[0].pos).with_y(0.0).length()).sum())));
            let (mut pitch, mut roll, mut vacc) = (0.0f32, 0.0f32, 0.0f32);
            for w in c.windows(3) {
                let (_, p, r) = w[1].rot.to_euler(EulerRot::YXZ);
                pitch = pitch.max(p.to_degrees().abs());
                roll = roll.max(r.to_degrees().abs());
                let (d0, d1) = ((w[1].t - w[0].t).max(1e-4), (w[2].t - w[1].t).max(1e-4));
                let a = ((w[2].pos.y - w[1].pos.y) / d1 - (w[1].pos.y - w[0].pos.y) / d0) / (0.5 * (d0 + d1));
                vacc = vacc.max(a.abs());
            }
            out.push(("car.max_pitch".into(), f(pitch)));
            out.push(("car.max_roll".into(), f(roll)));
            out.push(("car.max_vert_acc".into(), f(vacc)));
            if let Some(&b) = env_times("LIBERTY_BRAKE_AT").first() {
                let after: Vec<&CarSample> = c.iter().filter(|s| s.t >= b).collect();
                if let Some(s0) = after.first() {
                    out.push(("car.brake_from_kmh".into(), f(s0.kmh)));
                    if let Some(s1) = after.iter().find(|s| s.kmh.abs() < 1.0) {
                        out.push(("car.stop_secs".into(), f(s1.t - s0.t)));
                        out.push(("car.stop_dist".into(), f((s1.pos - s0.pos).with_y(0.0).length())));
                    }
                }
            }
            if let (Some(&cout), Some(back)) = (env_times("LIBERTY_CAR_OUT").first(), self.on_foot_after_car) {
                out.push(("car.exit_secs".into(), f(back - cout)));
            }
        }
        // the PC's own frame times (noisy: whatever else runs; never compared)
        let mut w = self.wall_ms.clone();
        if !w.is_empty() {
            w.sort_by(f32::total_cmp);
            out.push(("wall.frame_ms_mean".into(), f(w.iter().sum::<f32>() / w.len() as f32)));
            out.push(("wall.frame_ms_p95".into(), f(w[(w.len() * 95 / 100).min(w.len() - 1)])));
            out.push(("wall.frame_ms_max".into(), f(*w.last().unwrap())));
        }
        out.push(("frames".into(), self.niko.len().to_string()));
        out
    }
}

/// When the run ends: `<record dir>.json`.
pub fn write(mut exit: EventReader<AppExit>, mut lab: ResMut<Lab>) {
    if exit.read().next().is_none() || lab.written {
        return;
    }
    lab.written = true;
    let Some(dir) = std::env::args().skip_while(|a| a != "--record").nth(1) else { return };
    let body: Vec<String> = lab.numbers().iter().map(|(k, v)| format!("  \"{k}\": {v}")).collect();
    let path = format!("{}.json", dir.trim_end_matches(['/', '\\']));
    match std::fs::write(&path, format!("{{\n{}\n}}\n", body.join(",\n"))) {
        Ok(()) => info!("test lab: numbers in {path}"),
        Err(e) => error!("test lab: {path}: {e}"),
    }
}
