//! The balancer's pelvis control against IV's own: `out/vectors/pelvis_named.json`.

mod json;

use euphoria::footplace::{Body, Tuning};
use euphoria::math::Vec3;
use euphoria::pelvis::{update, State};
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

fn flag(j: &Json) -> bool {
    matches!(j, Json::Bool(true))
}

fn state(j: &Json) -> State {
    let tw = j.get("twist").arr();
    let st = j.get("stance").arr();
    State {
        offset: v3(j.get("offset")),
        vel: v3(j.get("vel")),
        anchor: v3(j.get("anchor")),
        drift: j.get("drift").f32(),
        front_i: j.get("front_i").f32(),
        side_i: j.get("side_i").f32(),
        twist: [tw[0].f32(), tw[1].f32()],
        lean_side: j.get("lean_side").f32(),
        lean_front: j.get("lean_front").f32(),
        stance: [flag(&st[0]), flag(&st[1])],
        use_vel: flag(j.get("use_vel")),
    }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 5e-5 * (1.0 + b.abs())
}

#[test]
fn pelvis_control_matches_iv() {
    let Some(dir) = vectors_dir() else { return };
    let path = dir.join("pelvis_named.json");
    if !path.exists() {
        return;
    }
    let v = json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut n = 0;
    for (i, c) in v.get("cases").arr().iter().enumerate() {
        let t = c.get("tuning");
        let nrm = t.get("normal").arr();
        let tuning = Tuning {
            up: v3(t.get("up")),
            dt: t.get("dt").f32(),
            twist_centre: t.get("twist_centre").f32(),
            twist_range: t.get("twist_range").f32(),
            lead_time: t.get("lead_time").f32(),
            front_base: t.get("front_base").f32(),
            normal: [v3(&nrm[0]), v3(&nrm[1])],
            ..Default::default()
        };
        let b = c.get("body");
        let mut body = Body {
            com: v3(b.get("com")),
            com_hvel: v3(b.get("com_hvel")),
            spin: v3(b.get("spin")),
            side: v3(b.get("side")),
            front: v3(b.get("front")),
            ..Default::default()
        };
        body.pelvis.b = v3(b.get("pelvis_b"));
        body.pelvis.c = v3(b.get("pelvis_c"));
        body.pelvis.pos = v3(b.get("pelvis_pos"));
        let fc = b.get("foot_c").arr();
        body.feet[0].frame.c = v3(&fc[0]);
        body.feet[1].frame.c = v3(&fc[1]);
        let mut st = state(c.get("state"));
        update(&mut st, &tuning, &body);
        let w = state(c.get("out"));
        assert!((st.offset - w.offset).length() <= 5e-5, "case {i}: offset");
        for (name, a, b) in [
            ("drift", st.drift, w.drift),
            ("front_i", st.front_i, w.front_i),
            ("side_i", st.side_i, w.side_i),
            ("twist0", st.twist[0], w.twist[0]),
            ("twist1", st.twist[1], w.twist[1]),
            ("lean_side", st.lean_side, w.lean_side),
            ("lean_front", st.lean_front, w.lean_front),
        ] {
            assert!(near(a, b), "case {i}: {name} {a} want {b}");
        }
        n += 1;
    }
    eprintln!("pelvis control: {n} cases match");
}
