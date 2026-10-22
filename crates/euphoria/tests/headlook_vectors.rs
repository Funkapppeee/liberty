//! headLook against IV's own routine: `out/vectors/headlook_cases.json` (400 random cases: spine
//! poses, target and body motion, starting spine twist, both flags; re/fit_headlook.py dump).

mod json;

use euphoria::headlook::{step, Input};
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

fn v3(j: &Json) -> Vec3 {
    let a = j.arr();
    Vec3::new(a[0].f32(), a[1].f32(), a[2].f32())
}

fn fl<const N: usize>(j: &Json) -> [f32; N] {
    let a = j.arr();
    std::array::from_fn(|k| a[k].f32())
}

#[test]
fn headlook_matches_iv() {
    let Some(dir) = vectors_dir() else { return };
    let path = dir.join("headlook_cases.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let cases: Json = json::parse(&text).unwrap();
    let (mut n, mut bad) = (0, Vec::new());
    for (k, c) in cases.arr().iter().enumerate() {
        let rows = c.get("spine3_rows").arr();
        let input = Input {
            spine3_rows: [v3(&rows[0]), v3(&rows[1]), v3(&rows[2])],
            spine3_pos: v3(c.get("spine3_pos")),
            head_row_a: v3(c.get("head_row_a")),
            eye_offset: c.get("eye_offset").f32(),
            cone: c.get("cone").f32(),
            up: v3(c.get("up")),
            body_vel: v3(c.get("body_vel")),
            body_spin: v3(c.get("body_spin")),
            target: v3(c.get("target")),
            target_vel: v3(c.get("target_vel")),
            strength: c.get("strength").f32(),
            damping: c.get("damping").f32(),
            twist_max_sum: c.get("twist_max_sum").f32(),
            neck_limits: fl(c.get("neck_limits")),
            head_limits: fl(c.get("head_limits")),
            spine3: fl(c.get("spine3")),
            spine2: fl(c.get("spine2")),
            force: c.get("force").bool(),
            world_up: c.get("world_up").bool(),
        };
        let iv = c.get("iv");
        let (front, out) = step(&input);
        let want_neck: [f32; 3] = fl(iv.get("neck"));
        let want_head: [f32; 3] = fl(iv.get("head"));
        let (s3, s2) = (iv.get("spine3").f32(), iv.get("spine2").f32());
        let (neck, head, os3, os2) = match out {
            Some(a) => (a.neck, a.head, a.spine3, a.spine2),
            None => ([0.0; 3], [0.0; 3], input.spine3[2], input.spine2[2]),
        };
        let err = (0..3)
            .map(|i| (neck[i] - want_neck[i]).abs().max((head[i] - want_head[i]).abs()))
            .fold((os3 - s3).abs().max((os2 - s2).abs()), f32::max);
        n += 1;
        if front != iv.get("in_front").bool() || err > 2e-4 {
            bad.push(format!("case {k}: front ours {front} IV {}, worst angle error {err:.6}", iv.get("in_front").bool()));
        }
    }
    assert!(n > 0, "no cases in {}", path.display());
    assert!(bad.is_empty(), "{} of {n} cases differ from IV:\n{}", bad.len(), bad.join("\n"));
}
