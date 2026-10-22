//! The balancer's balance check against IV's own code: `out/vectors/balfail.json`.

mod json;

use euphoria::balancer::{check, CheckParams};
use euphoria::math::Vec3;
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

fn v3(j: &json::Json) -> Vec3 {
    let a = j.arr();
    Vec3::new(a[0].f32(), a[1].f32(), a[2].f32())
}

#[test]
fn balance_check_matches_iv() {
    let Some(dir) = vectors_dir() else { return };
    let path = dir.join("balfail.json");
    if !path.exists() {
        return;
    }
    let v = json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let f = v.get("fields");
    let p = CheckParams { abort_ratio: f.get("0xc0").f32(), give_up_low: f.get("e0_byte").num() != 0.0 };
    let up = v3(f.get("up"));
    let mut n = 0;
    for (i, c) in v.get("cases").arr().iter().enumerate() {
        let out = check(&p, up, v3(c.get("footL")), v3(c.get("footR")), v3(c.get("com")), v3(c.get("com_vel")), Vec3::ZERO, Vec3::ZERO, c.get("prev_9ec").f32());
        let want_fail = matches!(c.get("failed"), json::Json::Bool(true));
        assert_eq!(out.failed, want_fail, "case {i}: failed");
        let s = c.get("stab").f32();
        assert!((out.stability - s).abs() <= 1e-4 * (1.0 + s.abs()), "case {i}: stability {} want {s}", out.stability);
        n += 1;
    }
    eprintln!("balance check: {n} cases match");
}
