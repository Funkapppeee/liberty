//! S1 closed-loop check: our body and OUR behaviour code (IV's balancer + bodyBalance, the ports whose
//! decisions match IV's), started from a real game moment, scored against what IV's own ped did next.
//! The open-loop replay (replay.rs) can't decide "stays up": IV's recorded targets are its balancer's
//! answers to IV's body, so fed to ours nothing corrects the first drift. Here our balancer answers
//! our body.
//!
//! Input: the live episodes the mini player saves (`out/s1/live/<utc>_<step>_<char>.txt`). For each:
//! our body starts in IV's pose (every joint at IV's angles, the body frame and COM IV's) with IV's
//! own velocities (pelvis velocity and spin, every joint's relative spin), on flat ground at the
//! episode's ground height, then runs on our balancer for as long as IV's episode lasted.
//! The start is IV's first tick; a hit or shove that lands a few ticks later (a jump in IV's COM
//! velocity in the first 0.3 s) is copied onto our parts as IV's own per-part velocity jump.
//! STAND-INS: every episode runs as a shove (balance), so shots
//! don't get the shot behaviour (the wound isn't in the recording) and car hits no car; the ground
//! is flat (stairs / wall episodes have no stairs or wall).
//!
//! Run: `liberty-view --closedloop out/s1/live/*.txt` (no window). Prints one line per episode and
//! the scorecard; writes `out/s1/closed/<episode>.csv` (both bodies' parts per IV tick, Bevy axes,
//! for drawing) and `out/s1/closed/scorecard.txt`.

use super::replay::{self, from_nm_v, rec_frame, rec_rel_com, tilt, Rec, Replay, LINKS};
use super::*;
use std::io::Write as _;
use std::path::PathBuf;

/// Fallen: the COM under this height over the ground, or the body tilted past `FALL_TILT`.
const FALL_COM: f32 = 0.5;
const FALL_TILT: f32 = 60.0;
/// A step: a foot more than this far (horizontal) from where it last stood; counted only until the
/// body tips past 45 degrees (a body on the ground rolls its feet about).
const STEP: f32 = 0.15;
/// A hit: IV's COM velocity changing more than this (m/s) from one tick to the next.
const HIT_JUMP: f32 = 0.3;

#[derive(Default, Clone, Copy)]
struct Score {
    fell: bool,
    /// When the tilt first passed 45 degrees.
    fall_t: Option<f32>,
    steps: u32,
    max_tilt: f32,
    min_com: f32,
}

/// Counts steps from a foot's horizontal positions.
#[derive(Default)]
struct Steps {
    anchor: [Option<Vec2>; 2],
    n: u32,
}

impl Steps {
    fn feed(&mut self, feet: [Vec3; 2]) {
        for s in 0..2 {
            let p = feet[s].xz();
            match self.anchor[s] {
                None => self.anchor[s] = Some(p),
                Some(a) if a.distance(p) > STEP => {
                    self.n += 1;
                    self.anchor[s] = Some(p);
                }
                _ => {}
            }
        }
    }
}

fn score_sample(sc: &mut Score, t: f32, tilt_deg: f32, com_h: f32) {
    sc.max_tilt = sc.max_tilt.max(tilt_deg);
    sc.min_com = sc.min_com.min(com_h);
    if tilt_deg > 45.0 && sc.fall_t.is_none() {
        sc.fall_t = Some(t);
    }
    if com_h < FALL_COM || tilt_deg > FALL_TILT {
        sc.fell = true;
    }
}

/// The step tag in a live episode's file name (`<utc>_<step>_<char>.txt`; older files have none).
fn step_of(path: &std::path::Path) -> String {
    let stem = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() >= 3 { parts[1..parts.len() - 1].join("_") } else { "untagged".into() }
}

/// Start our sim in IV's state at `r0` and hand it to our balancer.
fn start(def: &RagdollDef, tuning: Tuning, r0: &Rec, ground_gta: f32) -> RagdollSim {
    let rp = Replay::new(def, r0, ground_gta, false);
    let mut sim = rp.sim;
    // IV's velocities: the pelvis link's, and each joint's relative spin in its parent joint frame
    let iv_of_link = |sim: &RagdollSim, l: usize| (0..def.parts.len()).find(|&i| def.parts[i].body == i && sim.link[i] == l).and_then(|i| replay::iv_index(def, i));
    if let Some(k0) = iv_of_link(&sim, 0) {
        sim.body.root_vel = dv(from_nm_v(r0.links[k0].vel));
        sim.body.root_angvel = dv(from_nm_v(r0.links[k0].spin));
    }
    for l in 1..sim.body.links.len() {
        let parent = sim.body.links[l].joint.as_ref().map_or(0, |j| j.parent);
        if let (Some(k), Some(kp)) = (iv_of_link(&sim, l), iv_of_link(&sim, parent)) {
            let rel = dv(from_nm_v(r0.links[k].spin - r0.links[kp].spin));
            sim.body.u[l] = sim.body.joint_frame_parent(l).rot.inverse() * rel;
        }
    }
    sim.body.update_kinematics();
    // what the muscles hold: this pose; his right / backward from the shoulders as he stands now
    for (i, p) in def.parts.iter().enumerate() {
        if p.body == i && i != 0 {
            sim.base[i] = sim.body.angles(sim.link[i]).map(|a| a as f32);
        }
    }
    let d = sim.body(def.upper[1]).translation - sim.body(def.upper[0]).translation;
    let right = Vec3::new(d.x, 0.0, d.z).normalize_or(Vec3::X);
    sim.nm_rows = [right, right.cross(Vec3::Y)];
    sim.mode = Mode::Balance;
    sim.tuning = tuning;
    sim.nm_on = true;
    sim.mode_age = 0.0;
    sim.age = 0.0;
    let base = sim.base.clone();
    sim.muscles(def, &base);
    sim
}

/// The lowest corner of our two foot boxes (Bevy y = GTA z).
fn lowest_foot(def: &RagdollDef, sim: &RagdollSim) -> Option<f32> {
    let mut low = f32::MAX;
    for side in 0..2 {
        let f = def.feet[side];
        let pc::Shape::Box { half } = sim.colliders[f].shape else { return None };
        let w = sim.body.pose[sim.link[f]].mul(&sim.colliders[f].local);
        for c in 0..8 {
            let corner = DVec3::new(if c & 1 == 0 { -half.x } else { half.x }, if c & 2 == 0 { -half.y } else { half.y }, if c & 4 == 0 { -half.z } else { half.z });
            low = low.min(w.apply(corner).y as f32);
        }
    }
    Some(low)
}

pub fn run(def: &RagdollDef, tuning: Tuning, files: &[String]) -> anyhow::Result<()> {
    let out_dir = PathBuf::from("out/s1/closed");
    std::fs::create_dir_all(&out_dir)?;
    let mut paths: Vec<PathBuf> = files.iter().map(PathBuf::from).filter(|p| p.extension().is_some_and(|e| e == "txt")).collect();
    paths.sort();
    let mut lines = Vec::new();
    let mut table: Vec<(String, String, Score, Score)> = Vec::new();
    for path in &paths {
        let Ok((recs, live_ground)) = replay::load(path) else { continue };
        if recs.len() < 10 {
            continue;
        }
        let ground_gta = live_ground.unwrap_or_else(|| replay::ground_under(&recs));
        // the start tick: the fastest COM in the first 0.25 s
        // (LIBERTY_CLOSED_FASTEST: the old start, the fastest COM in the first 0.25 s, no hit copy)
        let win = if std::env::var("LIBERTY_CLOSED_FASTEST").is_ok() { 0.25 } else { 0.0 };
        let i0 = (0..recs.len()).filter(|&i| recs[i].t - recs[0].t <= win).max_by(|&a, &b| recs[a].com_vel.length().total_cmp(&recs[b].com_vel.length())).unwrap_or(0);
        let recs = &recs[i0..];
        let t0 = recs[0].t;
        let mut sim = start(def, tuning, &recs[0], ground_gta);
        // The ground right under our lower foot box (LIBERTY_CLOSED_FEET_OFF: IV's ground instead) (IV's feet are on the
        // ground at the start; ours, posed from IV's angles, hang a few mm above IV's ground)
        let ground_gta = if std::env::var("LIBERTY_CLOSED_FEET_OFF").is_err() { lowest_foot(def, &sim).unwrap_or(ground_gta) } else { ground_gta };
        sim.world_at = None;
        let ground = ChunkCollision::new(
            &{
                let c = from_nm_v(Vec3::new(recs[0].com.x, recs[0].com.y, ground_gta));
                let s = 50.0;
                [[c.x - s, c.y, c.z - s], [c.x + s, c.y, c.z - s], [c.x + s, c.y, c.z + s], [c.x - s, c.y, c.z + s]]
            },
            &[[0, 1, 2], [0, 2, 3]],
        );
        let ground_y = from_nm_v(Vec3::new(0.0, 0.0, ground_gta)).y;
        let (mut iv, mut ours) = (Score { min_com: f32::MAX, ..default() }, Score { min_com: f32::MAX, ..default() });
        let (mut iv_steps, mut our_steps) = (Steps::default(), Steps::default());
        let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        let mut csv = std::io::BufWriter::new(std::fs::File::create(out_dir.join(format!("{name}.csv")))?);
        writeln!(csv, "# t, then IV's 21 parts (x y z), then ours (x y z; nan where we have no such part), Bevy axes, ground at y {ground_y}")?;
        let mut t = 0.0f32;
        let dt = 1.0 / 60.0;
        // LIBERTY_CLOSED_TRACE=<part of a file name>: that episode's joints, IV's and ours, every 0.1 s
        let trace = std::env::var("LIBERTY_CLOSED_TRACE").is_ok_and(|v| name.contains(&v));
        let map = replay::joint_map(def);
        let mut next_trace = 0.0f32;
        let mut prev: Option<&Rec> = None;
        for r in recs {
            let rt = r.t - t0;
            while t < rt {
                sim.step(def, dt, [&ground], &[]);
                t += dt;
            }
            // the hit: in the first 0.3 s, a jump in IV's COM velocity (more than gravity and the
            // muscles give in one tick) is copied onto our parts, each part getting IV's own jump
            if let Some(p) = prev {
                let jump = (r.com_vel - p.com_vel).length();
                if rt <= 0.3 && jump > HIT_JUMP {
                    for l in 0..sim.body.links.len() {
                        let Some(k) = (0..def.parts.len()).find(|&i| def.parts[i].body == i && sim.link[i] == l).and_then(|i| replay::iv_index(def, i)) else { continue };
                        let dvel = dv(from_nm_v(r.links[k].vel - p.links[k].vel));
                        let (com, m) = (sim.body.com[l], sim.body.links[l].mass);
                        sim.body.apply_impulse(l, com, dvel * m);
                    }
                    if trace {
                        println!("  hit at {rt:.2}: IV's COM velocity jumped {jump:.2} m/s");
                    }
                }
            }
            prev = Some(r);
            // IV
            let rc = rec_rel_com(r);
            let iv_parts: Vec<Vec3> = (0..LINKS).map(|k| from_nm_v(r.com + r.links[k].pos - rc)).collect();
            score_sample(&mut iv, rt, rec_frame(r).map_or(0.0, tilt), r.com.z - ground_gta);
            if iv.fall_t.is_none() {
                iv_steps.feed([iv_parts[3], iv_parts[6]]);
            }
            // ours
            let our_parts: Vec<Option<Vec3>> = (0..LINKS).map(|k| sim.iv_part_com(def, k)).collect();
            score_sample(&mut ours, rt, sim.body_frame(def).map_or(0.0, tilt), sim.whole_com().y - ground_y);
            if let (Some(l), Some(rr), None) = (our_parts[3], our_parts[6], ours.fall_t) {
                our_steps.feed([l, rr]);
            }
            if trace && rt >= next_trace {
                next_trace = rt + 0.1;
                let j = |k: usize, i: usize| (r.joints[k].now[i], sim.replay_now(def, map[k]).map_or(f32::NAN, |a| a[i]));
                let f = |(a, b): (f32, f32)| format!("{a:5.2}/{b:5.2}");
                let touch = |side: usize| sim.touching.get(sim.link[def.feet[side]]).copied().unwrap_or(false) as u8;
                // our knee: its drive's target, strength, joint stiffness, carried inertia
                let knee = |side: usize| {
                    let l = sim.link[def.calves[side]];
                    let k = (0..3).find(|&k| def.parts[def.calves[side]].limits[k].is_some()).unwrap_or(0);
                    sim.body.drives[l].map_or("-".into(), |d| format!("want {:5.2} k {:5.0} c {:4.0} s {:.2} carried {:.2}", d.target[k], d.stiffness, d.damping, sim.body.stiffness[l], sim.body.carried[l].unwrap_or(0.0)))
                };
                println!("        our knee L {} | R {} || IV wants knee {:5.2} {:5.2} hip l1 {:5.2} {:5.2} ankle l1 {:5.2} {:5.2}", knee(0), knee(1), r.joints[1].want[0], r.joints[4].want[0], r.joints[0].want[0], r.joints[3].want[0], r.joints[2].want[0], r.joints[5].want[0]);
                println!(
                    "  t {rt:4.2} tilt {:5.1}/{:5.1} com {:.2}/{:.2} | hipL {} kneeL {} ankL {} | hipR {} kneeR {} ankR {} | spine0 {} | IV feet {}{} ours {}{}",
                    rec_frame(r).map_or(0.0, tilt), sim.body_frame(def).map_or(0.0, tilt), r.com.z - ground_gta, sim.whole_com().y - ground_y,
                    f(j(0, 0)), f(j(1, 0)), f(j(2, 0)), f(j(3, 0)), f(j(4, 0)), f(j(5, 0)), f(j(6, 0)),
                    r.contact[3] as u8, r.contact[6] as u8, touch(0), touch(1)
                );
            }
            let mut row = vec![format!("{rt:.4}")];
            for p in &iv_parts {
                row.push(format!("{:.4} {:.4} {:.4}", p.x, p.y, p.z));
            }
            for p in &our_parts {
                row.push(p.map_or("nan nan nan".into(), |p| format!("{:.4} {:.4} {:.4}", p.x, p.y, p.z)));
            }
            writeln!(csv, "{}", row.join(" "))?;
        }
        iv.steps = iv_steps.n;
        ours.steps = our_steps.n;
        let fmt_t = |s: &Score| s.fall_t.map_or("    -".into(), |t| format!("{t:5.2}"));
        let line = format!(
            "{name:36} {:5.1}s  IV {:4} tilt {:5.1} steps {:2} fall@{} | ours {:4} tilt {:5.1} steps {:2} fall@{}  {}",
            recs.last().unwrap().t - t0,
            if iv.fell { "FELL" } else { "up" },
            iv.max_tilt,
            iv.steps,
            fmt_t(&iv),
            if ours.fell { "FELL" } else { "up" },
            ours.max_tilt,
            ours.steps,
            fmt_t(&ours),
            if iv.fell == ours.fell { "" } else { "MISS" }
        );
        println!("{line}");
        lines.push(line);
        table.push((name, step_of(path), iv, ours));
    }
    // the scorecard
    let n = table.len();
    let agree = table.iter().filter(|(_, _, a, b)| a.fell == b.fell).count();
    let iv_up = table.iter().filter(|(_, _, a, _)| !a.fell).count();
    let both_up = table.iter().filter(|(_, _, a, b)| !a.fell && !b.fell).count();
    let iv_fell = n - iv_up;
    let both_fell = table.iter().filter(|(_, _, a, b)| a.fell && b.fell).count();
    let mut summary = vec![
        String::new(),
        format!("SCORECARD: ours matches IV (stays up / falls) in {agree} of {n} episodes"),
        format!("  IV stayed up in {iv_up}: ours too in {both_up}"),
        format!("  IV fell in {iv_fell}: ours too in {both_fell}"),
        "  by episode type (agree / episodes; misses: ours fell where IV stayed up | ours stayed up where IV fell):".into(),
    ];
    let mut kinds: Vec<String> = table.iter().map(|t| t.1.clone()).collect();
    kinds.sort();
    kinds.dedup();
    for k in &kinds {
        let rows: Vec<_> = table.iter().filter(|t| &t.1 == k).collect();
        let a = rows.iter().filter(|t| t.2.fell == t.3.fell).count();
        let fell_wrong = rows.iter().filter(|t| !t.2.fell && t.3.fell).count();
        let up_wrong = rows.iter().filter(|t| t.2.fell && !t.3.fell).count();
        summary.push(format!("    {k:12} {a:2} / {:2}   misses {fell_wrong} | {up_wrong}", rows.len()));
    }
    for l in &summary {
        println!("{l}");
    }
    let mut f = std::fs::File::create(out_dir.join("scorecard.txt"))?;
    for l in lines.iter().chain(&summary) {
        writeln!(f, "{l}")?;
    }
    Ok(())
}
