//! The balancer's foot placement against IV's own: `out/vectors/footplace_named.json` (every input it
//! reads, its state before and after).

mod json;

use euphoria::footplace::{update, Body, Foot, Frame, Hip, State, Tuning};
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

fn pair_v(j: &Json) -> [Vec3; 2] {
    [v3(&j.arr()[0]), v3(&j.arr()[1])]
}

fn pair_f(j: &Json) -> [f32; 2] {
    [j.arr()[0].f32(), j.arr()[1].f32()]
}

fn frame(j: &Json) -> Frame {
    Frame { a: v3(j.get("a")), b: v3(j.get("b")), c: v3(j.get("c")), pos: v3(j.get("pos")) }
}

fn state(j: &Json) -> State {
    State {
        pos: pair_v(j.get("pos")),
        mid: v3(j.get("mid")),
        start: v3(j.get("start")),
        prev_target: v3(j.get("prev_target")),
        prev_foot: v3(j.get("prev_foot")),
        height: pair_f(j.get("height")),
        need: pair_f(j.get("need")),
        steps_left: j.get("steps_left").num() as i32,
        stepping: j.get("stepping").num() as u8,
        flags: j.get("flags").num() as u8,
    }
}

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).length() <= 5e-5 * (1.0 + b.length())
}

#[test]
fn foot_placement_matches_iv() {
    let Some(dir) = vectors_dir() else { return };
    let path = dir.join("footplace_named.json");
    if !path.exists() {
        return;
    }
    let v = json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut n = 0;
    for (i, c) in v.get("cases").arr().iter().enumerate() {
        let t = c.get("tuning");
        let tuning = Tuning {
            up: v3(t.get("up")),
            forward: v3(t.get("forward")),
            reference: pair_v(t.get("reference")),
            normal: pair_v(t.get("normal")),
            dt: t.get("dt").f32(),
            min_separation: t.get("min_separation").f32(),
            side_offset: t.get("side_offset").f32(),
            lead_time: t.get("lead_time").f32(),
            support_margin: t.get("support_margin").f32(),
            gap_limit: t.get("gap_limit").f32(),
            reach: t.get("reach").f32(),
            ..Default::default()
        };
        let b = c.get("body");
        let foot = |j: &Json| Foot { frame: frame(j.get("frame")), vel: v3(j.get("vel")), ang_vel: v3(j.get("ang_vel")) };
        let hip = |j: &Json| Hip { frame: frame(j.get("frame")), half: pair_f(j.get("half")), centre: pair_f(j.get("centre")), current: pair_f(j.get("current")) };
        let contact = b.get("contact").arr();
        let body = Body {
            com: v3(b.get("com")),
            com_vel: v3(b.get("com_vel")),
            com_hvel: v3(b.get("com_hvel")),
            spin: Vec3::ZERO,
            side: v3(b.get("side")),
            front: v3(b.get("front")),
            pelvis: frame(b.get("pelvis")),
            feet: [foot(&b.get("feet").arr()[0]), foot(&b.get("feet").arr()[1])],
            hips: [hip(&b.get("hips").arr()[0]), hip(&b.get("hips").arr()[1])],
            contact: [matches!(contact[0], Json::Bool(true)), matches!(contact[1], Json::Bool(true))],
        };
        let mut st = state(c.get("state"));
        update(&mut st, &tuning, &body);
        let want = state(c.get("out"));
        assert_eq!((st.stepping, st.steps_left, st.flags), (want.stepping, want.steps_left, want.flags), "case {i}: step/count/flags");
        for (name, a, w) in [
            ("left", st.pos[0], want.pos[0]),
            ("right", st.pos[1], want.pos[1]),
            ("mid", st.mid, want.mid),
            ("start", st.start, want.start),
            ("prev_target", st.prev_target, want.prev_target),
            ("prev_foot", st.prev_foot, want.prev_foot),
        ] {
            assert!(close(a, w), "case {i}: {name} {a:?} want {w:?}");
        }
        for k in 0..2 {
            assert!((st.need[k] - want.need[k]).abs() <= 5e-5 * (1.0 + want.need[k]), "case {i}: need[{k}]");
            assert!((st.height[k] - want.height[k]).abs() <= 1e-5, "case {i}: height[{k}]");
        }
        n += 1;
    }
    eprintln!("foot placement: {n} cases match");
}
