//! catchFall against IV's own code: `out/vectors/catchfall_*.json` (16 cases × 20 frames).
//! Start draws bit-exact; per-frame decisions within 1e-4.

mod json;

use euphoria::catchfall::{CatchFall, Input, Params};
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

fn near(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol * (1.0 + b.abs())
}

#[test]
fn catchfall_matches_iv() {
    let Some(dir) = vectors_dir() else { return };
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("catchfall_") && (p.to_string_lossy().ends_with("_air.json") || p.to_string_lossy().ends_with("_hit.json"))).collect();
    files.sort();
    assert!(!files.is_empty(), "no catchfall vectors");
    let tol = 1e-4;
    let mut frames_checked = 0;
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let v = json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let st = v.get("start");
        let seed = st.get("rng").arr();
        let mut rng = Rng::new(seed[0].num() as u32, seed[1].num() as u32);
        let mut cf = CatchFall::start(Params::default(), &mut rng);
        assert_eq!(rng.draws, st.get("draws").num() as u64, "{name}: start draws");
        assert!(cf.m[0].to_bits() == st.get("m5c").f32().to_bits() && cf.m[1].to_bits() == st.get("m60").f32().to_bits(), "{name}: m");
        assert!(cf.side_lean.to_bits() == st.get("m64").f32().to_bits(), "{name}: s");
        let ground = v.get("ground").f32();
        let touch: Vec<String> = match &v {
            Json::Obj(kv) if kv.iter().any(|(k, _)| k == "touch") => v.get("touch").arr().iter().map(|t| t.str().to_string()).collect(),
            _ => Vec::new(),
        };
        let touching = [touch.iter().any(|t| t.starts_with("armL")), touch.iter().any(|t| t.starts_with("armR"))];
        let dt = v.get("dt").f32();
        for f in v.get("frames").arr() {
            let i = f.get("inputs");
            let com_vel = v3(i.get("com_vel"));
            let input = Input {
                fwd: v3(i.get("fwd")),
                face: v3(i.get("face")),
                up: v3(i.get("up")),
                com_vel,
                chest_vel: v3(i.get("chest_vel")),
                ke: 0.5 * com_vel.dot(com_vel),
                shoulder: [v3(i.get("shoulder_l")), v3(i.get("shoulder_r"))],
                hand: if touching.contains(&true) { [v3(i.get("hand_l")), v3(i.get("hand_r"))] } else { [Vec3::ZERO; 2] },
                touching,
                surface_vel: [Vec3::ZERO; 2],
                dt,
            };
            let out = cf.step(&input, |a, b| {
                let (da, db) = (a.z - ground, b.z - ground);
                (da * db <= 0.0 && da != db).then(|| a + (b - a) * (da / (da - db)))
            });
            let fr = f.get("frame").num();
            let chk = |what: &str, got: f32, want: f32| assert!(near(got, want, tol), "{name} f{fr} {what}: {got} want {want}");
            chk("gauge", cf.gauge, f.get("s4c").f32());
            chk("back", out.back, f.get("s48").f32());
            chk("side", out.side, f.get("s58").f32());
            let fd = v3(f.get("v30"));
            chk("F.y", out.fall_dir.y, fd.y);
            chk("F.z", out.fall_dir.z, fd.z);
            let sp = f.get("spine_0").arr();
            chk("spine lean1", out.spine[0].0, sp[0].f32());
            chk("spine lean2", out.spine[0].1, sp[1].f32());
            chk("hipL", out.hip[0], f.get("hip_left").arr()[0].f32());
            chk("kneeL", out.knee[0], f.get("knee_left").f32());
            chk("ankleL", out.ankle[0], f.get("ankle_left").arr()[0].f32());
            chk("reachL", cf.arms[0].reach, f.get("reachL").f32());
            chk("reachR", cf.arms[1].reach, f.get("reachR").f32());
            for k in 0..4 {
                chk(&format!("spine_{k} lean1"), out.spine[k].0, f.get(&format!("spine_{k}")).arr()[0].f32());
            }
            for (k, side) in ["left", "right"].into_iter().enumerate() {
                if let Some(w) = out.wrist[k] {
                    let want = f.get(&format!("wrist_{side}")).arr();
                    for a in 0..3 {
                        chk(&format!("wrist_{side}[{a}]"), w[a], want[a].f32());
                    }
                }
            }
            if let Json::Obj(kv) = f {
                if kv.iter().any(|(k, _)| k == "feedback") {
                    let fb: Vec<&str> = f.get("feedback").arr().iter().map(|x| x.str()).collect();
                    assert_eq!((out.feedback_0c, out.feedback_10), (fb.contains(&"0xc"), fb.contains(&"0x10")), "{name} f{fr} feedback");
                }
            }
            let pr = f.get("probes").arr();
            for k in 0..2 {
                let p = pr[k].arr();
                let (a, b) = (v3(&p[0]), v3(&p[1]));
                for (w, g, e) in [("probe0", out.probe[k].0, a), ("probe1", out.probe[k].1, b)] {
                    assert!(near(g.x, e.x, 1e-3) && near(g.y, e.y, 1e-3) && near(g.z, e.z, 1e-3), "{name} f{fr} arm{k} {w}: {g:?} want {e:?}");
                }
            }
            let ik = f.get("ik").arr();
            for k in 0..2 {
                let t = v3(ik[k].get("vec").get("2"));
                let h = out.hand_target[k];
                assert!(near(h.x, t.x, 1e-3) && near(h.y, t.y, 1e-3) && near(h.z, t.z, 1e-3), "{name} f{fr} arm{k} hand target {h:?} want {t:?}");
            }
            frames_checked += 1;
        }
    }
    eprintln!("catchFall: {frames_checked} frames match");
}
