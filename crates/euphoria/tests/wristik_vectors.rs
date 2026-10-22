//! Wrist IK against IV's own code: `out/vectors/wristik.json` (inputs and outputs of the elbow
//! prediction and the wrist aim, both hands).

mod json;

use euphoria::armik::Frame;
use euphoria::math::Vec3;
use euphoria::wristik::{aim, predict_elbow};
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

fn floats(j: &json::Json) -> Vec<f32> {
    j.arr().iter().map(|x| x.f32()).collect()
}

fn v3(a: &[f32], i: usize) -> Vec3 {
    Vec3::new(a[i], a[i + 1], a[i + 2])
}

fn frame(a: &[f32]) -> Frame {
    Frame { rows: [v3(a, 0), v3(a, 4), v3(a, 8)], pos: v3(a, 12) }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4 * (1.0 + b.abs())
}

#[test]
fn wristik_matches_iv() {
    let Some(dir) = vectors_dir() else { return };
    let path = dir.join("wristik.json");
    if !path.exists() {
        return;
    }
    let v = json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut n = 0;
    for (i, c) in v.get("cases").arr().iter().enumerate() {
        let pi = c.get("predict_in");
        let sd = floats(pi.get("shoulder_desc"));
        let ed = floats(pi.get("elbow_desc"));
        let sh = frame(&sd);
        let upper = (sh.pos - v3(&ed, 12)).length();
        let p = predict_elbow(&sh, [sd[0x1C], sd[0x1D], sd[0x1E]], [sd[0x19], sd[0x1A], sd[0x1B]], upper, pi.get("scale").f32());
        let want = floats(c.get("predict_out"));
        assert!(near(p.x, want[0]) && near(p.y, want[1]) && near(p.z, want[2]), "case {i} elbow {p:?} want {want:?}");
        let ai = c.get("aim_in");
        let out = aim(
            v3(&floats(ai.get("point")), 0),
            v3(&floats(ai.get("normal")), 0),
            v3(&floats(ai.get("elbow")), 0),
            v3(&floats(ai.get("shoulder_desc")), 12),
            &frame(&floats(ai.get("wrist_desc"))),
            ai.get("k").f32(),
            ai.get("flag").num() != 0.0,
            ai.get("weight").f32(),
        );
        let o = c.get("aim_out");
        let wa = floats(o.get("angles"));
        for k in 0..3 {
            assert!(near(out.wrist[k], wa[k]), "case {i} wrist[{k}] {} want {}", out.wrist[k], wa[k]);
        }
        assert!(near(out.ret, o.get("ret").f32()), "case {i} ret {} want {}", out.ret, o.get("ret").f32());
        n += 1;
    }
    eprintln!("wrist IK: {n} cases match");
}
