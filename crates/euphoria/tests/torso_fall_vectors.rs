//! Part 2 checks against the Reader's vectors (git-ignored `out/vectors/`):
//! - `brace_torso_{left,front}.json`: lean step (spine lean1 ×4, hip pitch) from last frame's b and s,
//!   and the decay on non-bracing frames. Bit-exact (plain f32, game order).
//! - `brace_fall.json`: the fall hand-off: choice, draw count and roll settings.

mod json;

use euphoria::fall::{self, Contacts, FallAction};
use euphoria::rng::Rng;
use euphoria::torso::Torso;
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

fn load(name: &str) -> Option<Json> {
    let p = vectors_dir()?.join(name);
    let text = std::fs::read_to_string(&p).ok()?;
    Some(json::parse(&text).expect("vector json"))
}

fn same(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits() || (a == 0.0 && b == 0.0)
}

#[test]
fn lean_step_matches_iv() {
    let dt = 1.0f32 / 60.0;
    let mut checked = 0;
    for case in ["brace_torso_left.json", "brace_torso_front.json"] {
        let Some(v) = load(case) else {
            eprintln!("skip: {case} not found");
            return;
        };
        let frames = v.get("frames").arr();
        for (i, f) in frames.iter().enumerate() {
            let mut t = Torso { bend: f.get("b_before").f32() };
            let lean = t.lean_step(f.get("s").f32(), dt);
            let want = f.get("spine_lean1").arr();
            for k in 0..4 {
                assert!(same(lean.spine_lean1[k], want[k].f32()), "{case} f{i} spine{k}: {} want {}", lean.spine_lean1[k], want[k].f32());
            }
            assert!(same(lean.hip_pitch, f.get("balancer_9d0").f32()), "{case} f{i} hip {} want {}", lean.hip_pitch, f.get("balancer_9d0").f32());
            // Without bracing b only decays: the next frame reads the decayed value.
            if !f.get("bracing").bool() {
                if let Some(next) = frames.get(i + 1) {
                    assert!(same(t.bend, next.get("b_before").f32()), "{case} f{i} decay {} want {}", t.bend, next.get("b_before").f32());
                }
            }
            checked += 1;
        }
    }
    assert!(checked > 0);
    eprintln!("lean step: {checked} frames bit-exact");
}

#[test]
fn fall_handoff_matches_iv() {
    let Some(v) = load("brace_fall.json") else {
        eprintln!("skip: brace_fall.json not found");
        return;
    };
    for (i, c) in v.arr().iter().enumerate() {
        let names: Vec<&str> = c.get("contact").arr().iter().map(|n| n.str()).collect();
        let contacts = Contacts {
            feet: names.iter().any(|n| n.ends_with(".foot")),
            above_feet: names.iter().any(|n| !n.ends_with(".foot")),
        };
        let coin = c.get("coin").num() as u32 == 1;
        // The vectors record the coin, not the generator state at the fall: use a seed whose
        // first coin is the recorded one.
        let mut seed = 1u32;
        let mut rng = loop {
            let r = Rng::new(seed, 0);
            if { let mut probe = r; probe.coin() } == coin {
                break r;
            }
            seed += 1;
        };
        let action = fall::on_fail(contacts, 12.0, &mut rng);
        assert_eq!(rng.draws, c.get("draws").num() as u64, "case {i}: draw count");
        let started: Vec<&str> = c.get("started").arr().iter().map(|n| n.str()).collect();
        match action {
            FallAction::RollDownStairs(r) => {
                assert_eq!(started, ["rollDownStairs"], "case {i}");
                let s = c.get("rollDownStairs");
                assert!(same(r.stiffness, s.get("+0x14").f32()));
                assert!(same(r.v1, s.get("+0x1c").f32()));
                assert!(same(r.v2, s.get("+0x20").f32()));
                assert!(same(r.v4, s.get("+0x28").f32()));
                assert!(same(r.v5, s.get("+0x2c").f32()));
                assert!(same(r.v6, s.get("+0x34").f32()));
                assert!(same(r.v7, s.get("+0x3c").f32()));
                let rf = s.get("+0x24").f32();
                assert!((0.2..0.8).contains(&rf), "case {i}: random factor {rf}");
            }
            FallAction::CatchFall(_) => assert!(!coin || (contacts.feet && !contacts.above_feet), "case {i}"),
        }
    }
    let cf = fall::catch_fall_settings(12.0);
    assert_eq!((cf.legs_stiffness, cf.torso_stiffness, cf.arms_stiffness), (6.0, 9.0, 15.0));
}
