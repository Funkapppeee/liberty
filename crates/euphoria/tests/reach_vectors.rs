//! Reach for wound against IV's own routines: `out/vectors/reach_choice.json` (hand choice, 6,808
//! cases: every part, side offsets, hand states, flags; re/vec_reach_choice.py) and
//! `out/vectors/reach.json` (480 random reach steps; re/vec_reach.py).

mod json;

use euphoria::math::Vec3;
use euphoria::reach::{choose, flips, hold, hold_strength, plan, start_timer, Hit, HoldIn, Part, Side, StepIn, Wounds};
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
    let text = std::fs::read_to_string(vectors_dir()?.join(name)).ok()?;
    Some(json::parse(&text).unwrap())
}

fn v3(j: &Json) -> Vec3 {
    let a = j.arr();
    Vec3::new(a[0].f32(), a[1].f32(), a[2].f32())
}

fn fl<const N: usize>(j: &Json) -> [f32; N] {
    let a = j.arr();
    std::array::from_fn(|k| a[k].f32())
}

fn part_of(i: f64) -> Option<Part> {
    if i < 0.0 { None } else { Part::from_iv_index(i as usize) }
}

fn close(a: Vec3, b: Vec3, tol: f32) -> bool {
    (a - b).length() <= tol * b.length().max(1.0)
}

#[test]
fn hand_choice_matches_iv() {
    let Some(doc) = load("reach_choice.json") else { return };
    let (mut n, mut bad) = (0, Vec::new());
    for (k, c) in doc.get("cases").arr().iter().enumerate() {
        let part = Part::from_iv_index(c.get("part").num() as usize).unwrap();
        let flags = c.get("flags").num() as u32;
        let busy = c.get("busy").arr();
        let cur = c.get("cur").arr();
        let bits0 = c.get("bits0").num() as u32;
        let mut w = Wounds {
            left: part_of(cur[0].num()),
            right: part_of(cur[1].num()),
            left_arm_hurt: bits0 & 8 != 0,
            right_arm_hurt: bits0 & 0x10 != 0,
            ..Default::default()
        };
        let hit = Hit {
            part,
            lateral: c.get("lateral").f32(),
            front: c.get("front").f32(),
            reach_for_wound: flags & 1 != 0,
            no_left: flags & 0x10 != 0,
            no_right: flags & 0x20 != 0,
            left_hand: busy[0].num() as u32,
            right_hand: busy[1].num() as u32,
        };
        let (l, r) = choose(&hit, &mut w);
        let bits = c.get("bits").num() as u32;
        let mut got = (l as u32) << 1 | (r as u32) << 2;
        got |= (w.left_arm_hurt as u32) << 3 | (w.right_arm_hurt as u32) << 4;
        got |= (w.left_leg_hit as u32) << 5 | (w.right_leg_hit as u32) << 6;
        let hi = c.get("bits_hi").num() as u32 & 2 != 0;
        let left_ok = if l { w.left == part_of(c.get("left").num()) && start_timer(Side::Left) == c.get("t_left").f32() } else { true };
        let right_ok = if r { w.right == part_of(c.get("right").num()) && (start_timer(Side::Right) - c.get("t_right").f32()).abs() < 1e-6 } else { true };
        n += 1;
        if got != bits || hi != w.from_behind || !left_ok || !right_ok {
            bad.push(format!("case {k}: bits {got:#x} want {bits:#x}, behind {} want {hi}", w.from_behind));
        }
    }
    assert!(n > 1000, "only {n} cases");
    assert!(bad.is_empty(), "{} of {n} differ: {:?}", bad.len(), &bad[..bad.len().min(8)]);
}

#[test]
fn reach_step_matches_iv() {
    let Some(doc) = load("reach.json") else { return };
    let (mut n, mut forces, mut bad) = (0, 0, Vec::new());
    for (k, c) in doc.get("cases").arr().iter().enumerate() {
        let side = if c.get("side").num() == 0.0 { Side::Left } else { Side::Right };
        let part = Part::from_iv_index(c.get("part").num() as usize).unwrap();
        let rows: Vec<Vec3> = c.get("part_rows").arr().iter().map(v3).collect();
        let pos = v3(c.get("part_pos"));
        let lp = fl::<3>(c.get("local"));
        let ln = fl::<3>(c.get("normal"));
        let sign = c.get("sign").f32();
        let wound = rows[0] * lp[0] + rows[1] * lp[1] + rows[2] * lp[2] + pos;
        let normal = (rows[0] * ln[0] + rows[1] * ln[1] + rows[2] * ln[2]) * sign;
        let flags = c.get("clav_flags").num() as u32;
        let inc = fl::<3>(c.get("clav_incoming")); // IV stores twist, lean1, lean2
        let input = StepIn {
            side,
            part,
            clavicle_limits: fl(c.get("clav_limits")),
            clavicle: fl(c.get("clav_before")),
            clavicle_locked: flags & 4 != 0,
            clavicle_incoming: if flags & 2 != 0 { Some([inc[1], inc[2], inc[0]]) } else { None },
            wound,
            normal,
            facing: v3(c.get("facing")),
            hand: v3(c.get("hand_pos")),
            pelvis: v3(c.get("pelvis")),
            spine3: v3(c.get("spine3")),
        };
        let p = plan(&input);
        let ik = c.get("armik");
        let wr = c.get("wrist");
        let want_clav = fl::<3>(c.get("clav_after"));
        let mut why = Vec::new();
        if (0..3).any(|j| (p.clavicle[j] - want_clav[j]).abs() > 2e-5) {
            why.push(format!("clavicle {:?} want {:?}", p.clavicle, want_clav));
        }
        if !close(p.target, v3(ik.get("target")), 2e-4) {
            why.push(format!("target {:?} want {:?}", p.target, v3(ik.get("target"))));
        }
        if (p.twist - ik.get("twist").f32()).abs() > 2e-4 || p.twist_scale != ik.get("xmm2").f32() {
            why.push(format!("twist {} {} want {} {}", p.twist, p.twist_scale, ik.get("twist").f32(), ik.get("xmm2").f32()));
        }
        if !close(p.wrist_point, v3(wr.get("point")), 2e-4) || !close(p.wrist_normal, v3(wr.get("normal")), 2e-4) {
            why.push("wrist request".into());
        }
        let flipped = flips(c.get("wrist_ret").f32(), part);
        let sign_after = if flipped { -sign } else { sign };
        if sign_after != c.get("sign_after").f32() {
            why.push(format!("flip: sign {sign_after} want {}", c.get("sign_after").f32()));
        }
        let strength = hold_strength(c.get("t190").f32(), c.get("t194").f32());
        if strength != c.get("hold").get("strength").f32() {
            why.push(format!("strength {strength}"));
        }
        let pv = c.get("part_vel");
        let f = hold(&HoldIn {
            wound: p.wrist_point,
            wound_vel: v3(pv),
            hand: v3(c.get("hand_pos")),
            hand_row_a: v3(&c.get("hand_rows").arr()[0]),
            hand_vel: v3(c.get("hand_vel")),
            sign: sign_after,
            strength,
            dt: c.get("dt").f32(),
        });
        let got: Vec<Vec3> = match c {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == "forces").map(|(_, v)| v.arr().iter().map(|g| v3(g.get("force"))).collect()).unwrap_or_default(),
            _ => vec![],
        };
        match (f, got.as_slice()) {
            (None, []) => {}
            (Some(f), [a, b]) if close(f, *a, 2e-4) && close(-f, *b, 2e-4) => forces += 1,
            _ => why.push(format!("hold force {f:?} want {got:?}")),
        }
        n += 1;
        if !why.is_empty() {
            bad.push(format!("case {k}: {}", why.join("; ")));
        }
    }
    assert!(n >= 400 && forces >= 50, "only {n} cases, {forces} with a hold force");
    assert!(bad.is_empty(), "{} of {n} differ: {:?}", bad.len(), &bad[..bad.len().min(6)]);
}
