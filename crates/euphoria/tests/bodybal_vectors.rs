//! bodyBalance against IV's own routine: `out/vectors/bodybal_sweep.json` (re/vec_bodybal.py sweep:
//! random hips, shoulders, elbows, foot and hand contacts, arms held, body tilt and spin, speed,
//! settings, timers and new balancer steps; one tick each).

mod json;

use euphoria::bodybalance::{Angles, Input, Settings, State};
use euphoria::math::Vec3;
use euphoria::rng::Rng;
use json::Json;
use std::path::PathBuf;

fn vectors_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("LIBERTY_VECTORS") {
        return Some(PathBuf::from(dir));
    }
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let p = dir.join("out").join("vectors");
        if p.exists() {
            return Some(p);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn v3(j: &Json) -> Vec3 {
    let a = j.arr();
    Vec3::new(a[0].f32(), a[1].f32(), a[2].f32())
}

fn fl<const N: usize>(j: &Json) -> [f32; N] {
    let a = j.arr();
    std::array::from_fn(|k| a[k].f32())
}

fn has(j: &Json, key: &str) -> bool {
    matches!(j, Json::Obj(kv) if kv.iter().any(|(k, _)| k == key))
}

fn settings(j: &Json) -> Settings {
    let mut s = Settings::default();
    let f = |k: &str, d: f32| if has(j, k) { j.get(k).f32() } else { d };
    s.body_stiffness = f("bodyStiffness", s.body_stiffness);
    s.elbow = f("elbow", s.elbow);
    s.shoulder = f("shoulder", s.shoulder);
    s.damping = f("damping", s.damping);
    s.spine_stiffness = f("spineStiffness?", s.spine_stiffness);
    s.spine_damping = f("spineDamping?", s.spine_damping);
    s.somersault_angle = f("somersaultAngle", s.somersault_angle);
    s.side_somersault_angle = f("sideSomersaultAngle", s.side_somersault_angle);
    s.arms_out_on_push = f("armsOutOnPush", 1.0) != 0.0;
    s.arms_out_on_push_multiplier = f("armsOutOnPushMultiplier", s.arms_out_on_push_multiplier);
    s.returning_to_balance_arms_out = f("returningToBalanceArmsOut", s.returning_to_balance_arms_out);
    s.arms_out_straighten_elbows = f("armsOutStraightenElbows", s.arms_out_straighten_elbows);
    s.arms_out_min_lean2 = f("armsOutMinLean2", s.arms_out_min_lean2);
    s.elbow_angle_on_contact = f("elbowAngleOnContact", s.elbow_angle_on_contact);
    s.bend_elbows_gait = f("bendElbowsGait", s.bend_elbows_gait);
    s
}

fn planted(flags: f32) -> bool {
    let b = flags as u32;
    b & 4 != 0 && b & 0x10 == 0
}

struct Check {
    n: usize,
    bad: Vec<String>,
}

impl Check {
    fn near(&mut self, case: usize, what: &str, got: f32, want: f32) {
        self.n += 1;
        if (got - want).abs() > 1e-4 * want.abs().max(1.0) {
            self.bad.push(format!("#{case} {what}: ours {got} IV {want}"));
        }
    }
}

#[test]
fn bodybalance_matches_iv() {
    let Some(dir) = vectors_dir() else { return };
    let Ok(text) = std::fs::read_to_string(dir.join("bodybal_sweep.json")) else { return };
    let data: Json = json::parse(&text).unwrap();
    let mut chk = Check { n: 0, bad: Vec::new() };
    let cases = data.get("cases").arr();
    for (k, c) in cases.iter().enumerate() {
        let s = settings(c.get("settings"));
        let si = c.get("state_in");
        let ji = c.get("joints_in");
        let want3 = |n: &str| -> Angles { fl(ji.get(n).get("want")) };
        let now3 = |n: &str| -> Angles { fl(ji.get(n).get("now")) };
        let rows = c.get("spine2_rows").arr();
        let vel = v3(c.get("vel"));
        let contact = c.get("contact");
        let hands = c.get("hands");
        let used = c.get("arm_used").arr();
        let input = Input {
            dt: 1.0 / 60.0,
            spine2_rows: [v3(&rows[0]), v3(&rows[1]), v3(&rows[2])],
            com_vel: vel,
            ke: 0.5 * vel.dot(vel),
            hip_want: [want3("hip_l"), want3("hip_r")],
            hip_now: [now3("hip_l"), now3("hip_r")],
            shoulder_now_lean1: [now3("shoulder_l")[0], now3("shoulder_r")[0]],
            elbow_now: [ji.get("elbow_l").get("now").f32(), ji.get("elbow_r").get("now").f32()],
            foot_planted: [planted(contact.get("l").f32()), planted(contact.get("r").f32())],
            hand_touching: hands.get("l").f32() as u32 & 4 != 0 || hands.get("r").f32() as u32 & 4 != 0,
            arm_held: [used[0].bool(), used[1].bool()],
            step: 5 + si.get("step").f32() as u32,
            clavicle_want: [want3("clavicle_l"), want3("clavicle_r")],
            shoulder_want: [want3("shoulder_l"), want3("shoulder_r")],
            elbow_want: [ji.get("elbow_l").get("want").f32(), ji.get("elbow_r").get("want").f32()],
        };
        let mut st = State {
            arms_out_timer: si.get("timer").f32(),
            arms_out_floor: si.get("bound").f32(),
            angles: fl(si.get("prev")),
            bend_timer: si.get("bend").f32(),
            elbow_value: si.get("elbow_rand").f32(),
            last_step: 5,
        };
        let r0 = c.get("rng0").arr();
        let mut rng = Rng::new(r0[0].num() as u32, r0[1].num() as u32);
        let out = st.step(&s, &input, &mut rng);

        let o = c.get("out");
        let iv3 = |n: &str| -> Angles { fl(o.get(n).get("want")) };
        for (j, name) in ["spine0", "spine1", "spine2", "spine3"].iter().enumerate() {
            for a in 0..3 {
                chk.near(k, &format!("{name}[{a}]"), out.spine[j][a], iv3(name)[a]);
            }
        }
        for a in 0..3 {
            chk.near(k, &format!("neck[{a}]"), out.neck[a], iv3("neck")[a]);
            chk.near(k, &format!("head[{a}]"), out.head[a], iv3("head")[a]);
        }
        for (side, sfx) in ["l", "r"].iter().enumerate() {
            for a in 0..3 {
                chk.near(k, &format!("clavicle_{sfx}[{a}]"), out.clavicle[side][a], iv3(&format!("clavicle_{sfx}"))[a]);
                chk.near(k, &format!("shoulder_{sfx}[{a}]"), out.shoulder[side][a], iv3(&format!("shoulder_{sfx}"))[a]);
                chk.near(k, &format!("wrist_{sfx}[{a}]"), out.wrist[side][a], iv3(&format!("wrist_{sfx}"))[a]);
            }
            chk.near(k, &format!("elbow_{sfx}"), out.elbow[side], o.get(&format!("elbow_{sfx}")).get("want").f32());
        }
        let g = |n: &str, i: usize| o.get(n).get("gains").arr()[i].f32();
        chk.near(k, "arm strength", out.arm_gain.strength, g("shoulder_l", 3));
        chk.near(k, "arm damping", out.arm_gain.damping, g("shoulder_l", 4));
        chk.near(k, "wrist strength", out.arm_gain.strength, g("wrist_r", 3));
        chk.near(k, "elbow strength", out.elbow_gain.strength, g("elbow_l", 2));
        chk.near(k, "elbow damping", out.elbow_gain.damping, g("elbow_l", 3));
        chk.near(k, "spine strength", out.spine_gain.strength, g("spine2", 3));
        chk.near(k, "spine damping", out.spine_gain.damping, g("spine2", 4));
        chk.near(k, "neck strength", out.neck_gain.strength, g("neck", 3));
        chk.near(k, "neck damping", out.neck_gain.damping, g("neck", 4));
        let so = c.get("state_out");
        chk.near(k, "elbow value", st.elbow_value, so.get("elbow_rand").f32());
        for a in 0..3 {
            chk.near(k, &format!("angle[{a}]"), st.angles[a], fl::<3>(so.get("prev"))[a]);
        }
        chk.near(k, "arms-out timer", st.arms_out_timer, so.get("timer").f32());
        chk.near(k, "bend timer", st.bend_timer, so.get("bend").f32());
        chk.near(k, "draws", rng.draws as f32, c.get("draws").f32());
        if has(c.get("tap"), "A") {
            chk.near(k, "arms-out amount", out.arms_out, c.get("tap").get("A").f32());
        }
    }
    println!("bodyBalance: {} values over {} cases, {} off", chk.n, cases.len(), chk.bad.len());
    assert!(chk.bad.is_empty(), "{} of {} off, first: {:?}", chk.bad.len(), chk.n, &chk.bad[..chk.bad.len().min(12)]);
}
