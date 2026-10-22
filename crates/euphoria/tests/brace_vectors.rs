//! Replays the Reader's vectors `out/vectors/writer_brace_part1.json` (git-ignored, found by
//! walking up from this crate, or at `$LIBERTY_VECTORS/writer_brace_part1.json`).
//! Draw counts, generator state and every checked value must match bit-exactly
//! (random draws, S, target velocity, timers and flags are all in the spec's bit-exact row).

mod json;

use euphoria::brace::{Brace, CharacterKind, FrameInput, Params};
use euphoria::math::Vec3;
use euphoria::rng::Rng;
use json::Json;
use std::path::PathBuf;

fn vectors_file() -> Option<PathBuf> {
    let name = "writer_brace_part1.json";
    if let Ok(dir) = std::env::var("LIBERTY_VECTORS") {
        let p = PathBuf::from(dir).join(name);
        return p.exists().then_some(p);
    }
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let p = dir.join("out").join("vectors").join(name);
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

fn rng_of(j: &Json) -> (u32, u32) {
    let a = j.arr();
    (a[0].num() as u32, a[1].num() as u32)
}

fn same(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits() || (a == 0.0 && b == 0.0)
}

/// Every predecessor of an MWC state (usually 2 or 3 of them).
fn mwc_preimages(x: u32, c: u32) -> Vec<(u32, u32)> {
    const MUL: u64 = 0x5CDC_FAA7;
    let t = ((c as u64) << 32) | x as u64;
    let mut out = Vec::new();
    let mut c_prev = t % MUL;
    while c_prev <= u32::MAX as u64 && c_prev <= t {
        let x_prev = (t - c_prev) / MUL;
        if x_prev <= u32::MAX as u64 {
            out.push((x_prev as u32, c_prev as u32));
        }
        c_prev += MUL;
    }
    out
}

/// The file gives the generator after Start. Walk back 5 or 6 steps and check that some seed,
/// run through our Start, reproduces the recorded A, B, h, s, w and lands on that state.
fn check_start(case: &Json, params: Params, target: Vec3) -> Result<(), String> {
    let st = case.get("start");
    let (x, c) = rng_of(st.get("rng"));
    let want = [
        st.get("A").f32(),
        st.get("B").f32(),
        st.get("h").f32(),
        st.get("s").f32(),
        st.get("w").f32(),
    ];
    let want_draws = if want[2] < 0.05 { 6 } else { 5 };
    let mut states = vec![(x, c)];
    for _ in 0..want_draws {
        states = states.iter().flat_map(|&(x, c)| mwc_preimages(x, c)).collect();
    }
    for (sx, sc) in states {
        let mut rng = Rng::new(sx, sc);
        let b = Brace::start(params, CharacterKind::Fred, target, &mut rng);
        let got = [b.spread_a, b.spread_b, b.start.h, b.start.s, b.start.w];
        if got.iter().zip(want).all(|(g, w)| same(*g, w)) && (rng.x, rng.c) == (x, c) {
            if rng.draws != want_draws {
                return Err(format!("Start took {} draws, want {want_draws}", rng.draws));
            }
            return Ok(());
        }
    }
    Err("no seed reproduces the Start draws".into())
}

fn run_case(case: &Json) -> Result<usize, String> {
    let dt = case.get("dt").f32();
    let pj = case.get("params");
    let params = Params {
        body_stiffness: pj.get("bodyStiffness").f32(),
        brace_distance: pj.get("braceDistance").f32(),
        target_prediction_time: pj.get("targetPredictionTime").f32(),
        reach_absorbtion_time: pj.get("reachAbsorbtionTime").f32(),
        time_to_backwards_brace: pj.get("timeToBackwardsBrace").f32(),
    };
    let start_target = v3(pj.get("pos"));
    check_start(case, params, start_target).map_err(|e| format!("Start: {e}"))?;

    let inp = case.get("inputs");
    let st = case.get("start");
    let (x, c) = rng_of(st.get("rng"));
    let mut rng = Rng::new(x, c);
    // The generator is already past Start; rebuild the state with the recorded Start values.
    let mut brace = Brace::start(params, CharacterKind::Fred, start_target, &mut Rng::new(0, 0));
    brace.spread_a = st.get("A").f32();
    brace.spread_b = st.get("B").f32();
    brace.start.h = st.get("h").f32();
    brace.start.s = st.get("s").f32();
    brace.start.w = st.get("w").f32();

    let base = FrameInput {
        target: start_target,
        com: v3(inp.get("com")),
        com_vel: v3(inp.get("com_vel")),
        up: v3(inp.get("up")),
        facing: v3(inp.get("F")),
        spine2_pos: v3(inp.get("spine2_pos")),
        spine2_side: v3(inp.get("spine2_row2")),
        shoulder_l: v3(inp.get("shoulderL")),
        shoulder_r: v3(inp.get("shoulderR")),
        arm_unavailable: [false, false],
        dt,
    };

    let frames = case.get("frames").arr();
    for (i, f) in frames.iter().enumerate() {
        let input = FrameInput { target: v3(f.get("pos")), ..base };
        let before = rng.draws;
        let out = brace.step(&input, &mut rng);
        let mut errs = Vec::new();
        let draws = rng.draws - before;
        let want_draws = f.get("draws").num() as u64;
        if draws != want_draws {
            errs.push(format!("draws {draws} want {want_draws}"));
        }
        if (rng.x, rng.c) != rng_of(f.get("rng")) {
            errs.push(format!("rng ({}, {}) want {:?}", rng.x, rng.c, rng_of(f.get("rng"))));
        }
        let tv = v3(f.get("targetVel"));
        let v = out.target_vel;
        if !(same(v.x, tv.x) && same(v.y, tv.y) && same(v.z, tv.z)) {
            errs.push(format!("targetVel {v:?} want {tv:?}"));
        }
        let floats = [
            ("S", out.smoothed),
            ("lookTimer", out.look_timer),
            ("A", brace.spread_a),
            ("B", brace.spread_b),
            ("D", out.delay),
            ("timeSinceStart", brace.time_since_start),
        ];
        for (key, got) in floats {
            let want = f.get(key).f32();
            if !same(got, want) {
                errs.push(format!("{key} {got:e} want {want:e}"));
            }
        }
        let bools = [
            ("bracing", out.bracing),
            ("armL", out.arm_l),
            ("armR", out.arm_r),
            ("lookAhead", out.look_ahead),
        ];
        for (key, got) in bools {
            if got != f.get(key).bool() {
                errs.push(format!("{key} {got}"));
            }
        }
        if !errs.is_empty() {
            return Err(format!("frame {i}: {}", errs.join("; ")));
        }
    }
    Ok(frames.len())
}

#[test]
fn brace_part1_vectors() {
    let Some(path) = vectors_file() else {
        eprintln!("SKIPPED: writer_brace_part1.json not found (it is git-ignored; set LIBERTY_VECTORS)");
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap();
    let root = json::parse(&text).expect("valid JSON");
    let cases = root.get("cases").arr();
    let mut failed = 0;
    for case in cases {
        let name = case.get("case").str();
        match run_case(case) {
            Ok(n) => eprintln!("PASS {name} ({n} frames)"),
            Err(e) => {
                failed += 1;
                eprintln!("FAIL {name}: {e}");
            }
        }
    }
    eprintln!("{} of {} cases pass", cases.len() - failed, cases.len());
    assert_eq!(failed, 0, "{failed} case(s) failed");
}
