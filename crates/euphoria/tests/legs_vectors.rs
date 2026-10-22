//! The balancer's leg solve (with its leg IK) against IV's own: `out/vectors/legs_named.json`.

mod json;

use euphoria::footplace::{Foot, Frame};
use euphoria::legs::{solve, Body, Hip, Knee, Pelvis, Placement, Tuning};
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

fn fs<const N: usize>(j: &Json) -> [f32; N] {
    let a = j.arr();
    std::array::from_fn(|i| a[i].f32())
}

fn frame(j: &Json) -> Frame {
    Frame { a: v3(j.get("a")), b: v3(j.get("b")), c: v3(j.get("c")), pos: v3(j.get("pos")) }
}

fn part(j: &Json) -> Foot {
    Foot { frame: frame(j.get("frame")), vel: v3(j.get("vel")), ang_vel: v3(j.get("ang_vel")) }
}

fn hip(j: &Json) -> Hip {
    let cur: [f32; 2] = fs(j.get("current"));
    Hip {
        frame: frame(j.get("frame")),
        strength: j.get("strength").f32(),
        damping: j.get("damping").f32(),
        twist_limit: j.get("twist_limit").f32(),
        half: fs(j.get("half")),
        centre: fs(j.get("centre")),
        centre_twist: j.get("centre_twist").f32(),
        desired: fs(j.get("desired")),
        current: [cur[0], cur[1], j.get("current_twist").f32()],
    }
}

fn knee(j: &Json) -> Knee {
    Knee { frame: frame(j.get("frame")), parent: frame(j.get("parent")), desired: j.get("desired").f32(), current: j.get("current").f32() }
}

fn pair<T>(j: &Json, f: impl Fn(&Json) -> T) -> [T; 2] {
    let a = j.arr();
    [f(&a[0]), f(&a[1])]
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 2e-4 * (1.0 + b.abs())
}

#[test]
fn leg_solve_matches_iv() {
    run("legs_named.json", 2e-4);
}

/// The same solve on blocks read from the REAL game's memory (a balancing pedestrian in the
/// session recording, `re/bal_real_case.py`). The wanted values are the targets the game had
/// stored from its previous update, so they agree to about 1e-3, not exactly. This is the test
/// that caught the emulator's wrong matrix-product stand-in (2 Oct 2026).
#[test]
fn leg_solve_matches_the_real_game() {
    run("legs_real_named.json", 1.5e-3);
}

fn run(file: &str, tol: f32) {
    let Some(dir) = vectors_dir() else { return };
    let path = dir.join(file);
    if !path.exists() {
        return;
    }
    let v = json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut n = 0;
    let mut worst = 0.0f32;
    for (i, c) in v.get("cases").arr().iter().enumerate() {
        let t = c.get("tuning");
        let tuning = Tuning {
            up: v3(t.get("up")),
            normal: pair(t.get("normal"), v3),
            ground_vel: v3(t.get("ground_vel")),
            hip_height: t.get("hip_height").f32(),
            hip_spread: t.get("hip_spread").f32(),
            straightness: t.get("straightness").f32(),
            step_predict: t.get("step_predict").f32(),
            reach: t.get("reach").f32(),
        };
        let b = c.get("body");
        let body = Body {
            com_vel: v3(b.get("com_vel")),
            front: v3(b.get("front")),
            pelvis: frame(b.get("pelvis")),
            pelvis_vel: v3(b.get("pelvis_vel")),
            pelvis_spin: v3(b.get("pelvis_spin")),
            hips: pair(b.get("hips"), hip),
            knees: pair(b.get("knees"), knee),
            ankles: pair(b.get("ankles"), |j| fs::<2>(j)),
            calves: pair(b.get("calves"), part),
            feet: pair(b.get("feet"), part),
            limbs: pair(b.get("limbs"), frame),
        };
        let p = c.get("placement");
        let placement = Placement { pos: pair(p.get("pos"), v3), mid: v3(p.get("mid")), flags: p.get("flags").num() as u8 };
        let q = c.get("pelvis");
        let pelvis = Pelvis {
            offset: v3(q.get("offset")),
            drift: q.get("drift").f32(),
            twist: fs(q.get("twist")),
            height: q.get("height").f32(),
            lean_side: q.get("lean_side").f32(),
            lean_front: q.get("lean_front").f32(),
        };
        let out = solve(&tuning, &body, &placement, &pelvis);
        let w = c.get("out");
        let wh: [[f32; 3]; 2] = pair(w.get("hip"), |j| fs::<3>(j));
        let wk: [f32; 2] = fs(w.get("knee"));
        let wa: [[f32; 2]; 2] = pair(w.get("ankle"), |j| fs::<2>(j));
        let mut check = |name: &str, a: f32, b: f32| {
            worst = worst.max((a - b).abs());
            // LIBERTY_VEC_PRINT: list every value instead of stopping at the first difference (for
            // cases taken from the real game's memory, which are a frame apart from their answers)
            if std::env::var("LIBERTY_VEC_PRINT").is_ok() {
                eprintln!("case {i}: {name:12} ours {a:9.4}  game {b:9.4}  diff {:8.4}", a - b);
                return;
            }
            assert!((a - b).abs() <= tol * (1.0 + b.abs()), "{file} case {i}: {name} {a} want {b}");
        };
        for s in 0..2 {
            for k in 0..3 {
                check(&format!("hip[{s}][{k}]"), out.hip[s][k], wh[s][k]);
            }
            check(&format!("knee[{s}]"), out.knee[s], wk[s]);
            for k in 0..2 {
                check(&format!("ankle[{s}][{k}]"), out.ankle[s][k], wa[s][k]);
            }
        }
        check("spine_lean", out.spine_lean, w.get("spine_lean").f32());
        check("height", out.height, w.get("height").f32());
        n += 1;
    }
    eprintln!("leg solve, {file}: {n} cases match (worst difference {worst:.2e})");
}
