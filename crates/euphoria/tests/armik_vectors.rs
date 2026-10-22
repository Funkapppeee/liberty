//! Two-bone arm IK against IV's own solver: `out/vectors/armik_solve.json` (every input the solver
//! reads, and its outputs).

mod json;

use euphoria::armik::{solve, Elbow, Frame, Request, Shoulder};
use euphoria::math::Vec3;
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

fn floats(j: &Json) -> Vec<f32> {
    j.arr().iter().map(|x| x.f32()).collect()
}

fn v3(a: &[f32], i: usize) -> Vec3 {
    Vec3::new(a[i], a[i + 1], a[i + 2])
}

/// Rows at +0x00/+0x10/+0x20 and position at +0x30 of a 4×4 block starting at float `at`.
fn frame(a: &[f32], at: usize) -> Frame {
    Frame { rows: [v3(a, at), v3(a, at + 4), v3(a, at + 8)], pos: v3(a, at + 12) }
}

#[test]
fn armik_matches_iv() {
    let Some(dir) = vectors_dir() else { return };
    let path = dir.join("armik_solve.json");
    if !path.exists() {
        return;
    }
    let v = json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut n = 0;
    let mut worst = 0.0f32;
    for (i, c) in v.get("solves").arr().iter().enumerate() {
        let sd = floats(c.get("shoulder_desc"));
        let ed = floats(c.get("elbow_desc"));
        let hand = floats(c.get("hand_block"));
        let part = floats(c.get("part_block"));
        let sc = c.get("scalars");
        let s = |k: u32| sc.get(&k.to_string()).f32();
        let flags = c.get("flags").num() as u32;
        let target = floats(c.get("target"));
        let shoulder = Shoulder {
            frame: frame(&sd, 0),
            muscle_damping: sd[0x14],
            muscle_strength: sd[0x15],
            half_range: [sd[0x17], sd[0x18], sd[0x16]],
            centre: [sd[0x19], sd[0x1A], sd[0x1B]],
            prev: [sd[0x1C], sd[0x1D], sd[0x1E]],
            limiter_prev: [sd[0x1F], sd[0x20], sd[0x21]],
        };
        let elbow = Elbow { frame: frame(&ed, 16), prev: ed[0x28], limiter_prev: ed[0x29] };
        let req = Request {
            target: v3(&target, 0),
            limb: frame(&floats(c.get("limb")), 0),
            hand: v3(&hand, 12),
            base_vel: v3(&part, 16),
            base_ang_vel: v3(&part, 20),
            reference_vel: if flags & 1 != 0 { Some(v3(&floats(c.get("vel")), 0)) } else { None },
            lead: s(7),
            twist: s(8),
            twist_scale: s(9),
            elbow_scale: s(10),
            blend: s(11),
            limiter: if flags & 4 != 0 { Some((s(12), s(13))) } else { None },
            no_overflow: flags & 2 != 0,
        };
        let out = solve(&req, &shoulder, &elbow);
        let o = c.get("out");
        let want_sh = floats(o.get("shoulder"));
        let want_t = floats(o.get("target"));
        let checks = [
            ("target.x", out.target.x, want_t[0]),
            ("target.y", out.target.y, want_t[1]),
            ("target.z", out.target.z, want_t[2]),
            ("elbow", out.elbow, o.get("elbow").f32()),
            ("lean1", out.shoulder[0], want_sh[0]),
            ("lean2", out.shoulder[1], want_sh[1]),
            ("twist", out.shoulder[2], want_sh[2]),
        ];
        for (what, got, want) in checks {
            let err = (got - want).abs() / (1.0 + want.abs());
            worst = worst.max(err);
            assert!(err <= 1e-4, "solve {i} (opt {}, flags {flags:#x}) {what}: {got} want {want}", c.get("opt").num());
        }
        n += 1;
    }
    eprintln!("arm IK: {n} solves match (worst relative error {worst:.2e})");
}
