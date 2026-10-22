//! Playing GTA IV's own animations on a skeleton: clips from `rage::anim`, bound to joints by
//! bone ID, sampled with interpolation, and blended idle → walk → run → sprint by speed with a
//! shared cycle phase so the feet stay in step while blending.

use std::collections::HashMap;

use bevy::prelude::*;
use rage::anim::{Animation, TrackKind};

use crate::assets::to_bevy;

pub struct Clip {
    pub name: String,
    pub duration: f32,
    /// Per joint: rotation keys (Bevy space) and translation keys, if the clip animates it.
    rot: Vec<Option<Vec<Quat>>>,
    pos: Vec<Option<Vec<Vec3>>>,
    /// Ground speed from the mover track (m/s); 0 for in-place clips.
    pub speed: f32,
    /// Root motion per frame, relative to frame 0, in the character's frame (Bevy axes).
    pub mover_pos: Vec<Vec3>,
    pub mover_rot: Vec<Quat>,
}

/// A clip's average root-bone heading (yaw about Bevy's up), if it animates the root.
fn mean_root_yaw(c: &Clip) -> Option<f32> {
    let k = c.rot.first()?.as_ref()?;
    let (s, co) = k.iter().fold((0.0f32, 0.0f32), |(s, co), q| {
        let y = q.to_euler(EulerRot::YXZ).0;
        (s + y.sin(), co + y.cos())
    });
    (!k.is_empty()).then(|| s.atan2(co))
}

/// Move a clip's root track into the gait clips' root frame when it's authored in another one:
/// IV's idle keeps its root at 0° while walk/run/sprint sit at ~85°, which left him standing
/// side-on whenever he stopped. Rotates the root's rotation and position keys about up.
fn align_root(c: &mut Clip, gait_yaw: f32) {
    let Some(y) = mean_root_yaw(c) else { return };
    let mut d = gait_yaw - y;
    while d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    }
    while d < -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    if d.abs() < 45f32.to_radians() {
        return;
    }
    let r = Quat::from_rotation_y(d);
    if let Some(Some(k)) = c.rot.first_mut() {
        for q in k.iter_mut() {
            *q = (r * *q).normalize();
        }
    }
    if let Some(Some(k)) = c.pos.first_mut() {
        for v in k.iter_mut() {
            *v = r * *v;
        }
    }
    info!("clip {}: root turned {:.0}° into the gait frame", c.name, d.to_degrees());
}

fn to_bevy_quat([x, y, z, w]: [f32; 4]) -> Quat {
    Quat::from_xyzw(x, z, -y, w).normalize()
}

impl Clip {
    pub fn new(anim: &Animation, joint_of_id: &HashMap<u16, usize>, joints: usize) -> Clip {
        let mut rot = vec![None; joints];
        let mut pos = vec![None; joints];
        let mut speed = 0.0;
        let (mut mover_pos, mut mover_rot) = (Vec::new(), Vec::new());
        for t in &anim.tracks {
            match t.kind {
                TrackKind::Rotation => {
                    if let Some(&j) = joint_of_id.get(&t.bone_id) {
                        rot[j] = Some(t.frames.iter().map(|&q| to_bevy_quat(q)).collect());
                    }
                }
                TrackKind::Translation => {
                    if let Some(&j) = joint_of_id.get(&t.bone_id) {
                        pos[j] = Some(t.frames.iter().map(|v| Vec3::from(to_bevy([v[0], v[1], v[2]]))).collect());
                    }
                }
                TrackKind::MoverRotation => {
                    let q0 = t.frames.first().map(|&q| to_bevy_quat(q)).unwrap_or(Quat::IDENTITY);
                    mover_rot = t.frames.iter().map(|&q| q0.inverse() * to_bevy_quat(q)).collect();
                }
                TrackKind::MoverTranslation => {
                    let p0 = t.frames.first().map(|v| Vec3::from(to_bevy([v[0], v[1], v[2]]))).unwrap_or_default();
                    mover_pos = t.frames.iter().map(|v| Vec3::from(to_bevy([v[0], v[1], v[2]])) - p0).collect();
                    if let (Some(a), Some(b)) = (t.frames.first(), t.frames.last()) {
                        let d = Vec3::new(b[0] - a[0], b[1] - a[1], 0.0).length();
                        if anim.duration > 0.0 {
                            speed = d / anim.duration;
                        }
                    }
                }
                _ => {}
            }
        }
        // IV's mover translation is kept in the mover's own, turning frame, not in the frame the
        // clip starts in: runstart_l_180 turns 180° and its track still runs +2.7 m "ahead", and
        // runstart_r_90 keeps going straight ahead after its turn. Read as start-frame offsets
        // he ran backwards through a 180° start (the moonwalk) and crabbed sideways through a
        // 90° one. So each frame's step is turned by the mover's rotation at that point (the
        // halfway rotation between the two frames: our choice, the step per frame is at most ~10°).
        if mover_rot.len() == mover_pos.len() && mover_pos.len() > 1 && std::env::var("LIBERTY_OLD_MOVE").is_err() {
            let mut acc = Vec::with_capacity(mover_pos.len());
            acc.push(Vec3::ZERO);
            for i in 1..mover_pos.len() {
                let r = mover_rot[i - 1].slerp(mover_rot[i], 0.5);
                acc.push(acc[i - 1] + r * (mover_pos[i] - mover_pos[i - 1]));
            }
            mover_pos = acc;
        }
        Clip { name: anim.name.clone(), duration: anim.duration.max(1.0 / 30.0), rot, pos, speed, mover_pos, mover_rot }
    }

    fn key(&self, phase: f32, n: usize) -> (usize, usize, f32) {
        let f = phase.rem_euclid(1.0) * (n.saturating_sub(1)) as f32;
        let a = f.floor() as usize;
        (a.min(n - 1), (a + 1).min(n - 1), f - f.floor())
    }

    /// Joint rotation at `phase` (0..1 of the cycle), if animated.
    pub fn rot(&self, j: usize, phase: f32) -> Option<Quat> {
        let k = self.rot[j].as_ref()?;
        if k.is_empty() {
            return None;
        }
        let (a, b, t) = self.key(phase, k.len());
        Some(k[a].slerp(k[b], t))
    }

    /// Root motion at time 	 seconds into a one-shot clip: (offset, rotation) since frame 0.
    pub fn mover_at(&self, t: f32) -> (Vec3, Quat) {
        let phase = (t / self.duration).clamp(0.0, 1.0);
        let p = if self.mover_pos.is_empty() {
            Vec3::ZERO
        } else {
            let (a, b, f) = self.key_clamped(phase, self.mover_pos.len());
            self.mover_pos[a].lerp(self.mover_pos[b], f)
        };
        let r = if self.mover_rot.is_empty() {
            Quat::IDENTITY
        } else {
            let (a, b, f) = self.key_clamped(phase, self.mover_rot.len());
            self.mover_rot[a].slerp(self.mover_rot[b], f)
        };
        (p, r)
    }

    fn key_clamped(&self, phase: f32, n: usize) -> (usize, usize, f32) {
        let f = phase.clamp(0.0, 1.0) * (n.saturating_sub(1)) as f32;
        let a = f.floor() as usize;
        (a.min(n - 1), (a + 1).min(n - 1), f - f.floor())
    }

    /// Full pose of a one-shot clip at 	 seconds (no looping).
    pub fn pose_at(&self, t: f32, rest: &[Transform]) -> Vec<(Quat, Vec3)> {
        let phase = (t / self.duration).clamp(0.0, 0.9999);
        rest.iter()
            .enumerate()
            .map(|(j, r)| (self.rot(j, phase).unwrap_or(r.rotation), self.pos(j, phase).unwrap_or(r.translation)))
            .collect()
    }

    pub fn pos(&self, j: usize, phase: f32) -> Option<Vec3> {
        let k = self.pos[j].as_ref()?;
        if k.is_empty() {
            return None;
        }
        let (a, b, t) = self.key(phase, k.len());
        Some(k[a].lerp(k[b], t))
    }
}

/// The player's locomotion set, slowest first.
pub struct Locomotion {
    pub clips: Vec<Clip>,
    /// Every other clip in the pack (starts, stops, turns...), by name.
    pub extra: HashMap<String, Clip>,
    /// Turn-lean versions of walk/run/sprint (tiers 1..3; tier 0 = idle has none), each with the
    /// yaw rate its own root motion turns at (rad/s, + = left), sorted. IV blends these by how fast
    /// the ped is turning (moveblend.dat: groups without them "will not lean as they take turns").
    pub turns: Vec<Vec<(f32, Clip)>>,
}

impl Locomotion {
    /// idle, walk, run, sprint from `move_player.wad`.
    pub fn from_anims(anims: &[Animation], joint_of_id: &HashMap<u16, usize>, joints: usize) -> Option<Locomotion> {
        let find = |n: &str| anims.iter().find(|a| a.name == n).map(|a| Clip::new(a, joint_of_id, joints));
        // idle + walk are required; a missing run/sprint falls back to the next slower clip
        let mut clips = vec![
            find("idle")?,
            find("walk")?,
            find("run").or_else(|| find("walk"))?,
            find("sprint").or_else(|| find("run")).or_else(|| find("walk"))?,
        ];
        // sensible speeds if a clip has no mover track
        for (c, fallback) in clips.iter_mut().zip([0.0, 1.6, 4.7, 7.2]) {
            if c.speed < 0.05 {
                c.speed = fallback;
            }
        }
        clips[0].speed = 0.0;
        let gait_yaw = mean_root_yaw(&clips[1]).unwrap_or(0.0);
        align_root(&mut clips[0], gait_yaw);
        if std::env::var("LIBERTY_RAG_DEBUG").is_ok() {
            for c in &clips {
                let r = c.rot(0, 0.0).unwrap_or(Quat::IDENTITY);
                let (y, x, z) = r.to_euler(EulerRot::YXZ);
                let mv = c.mover_at(0.0).1.to_euler(EulerRot::YXZ).0;
                if let Some(k) = c.rot[0].as_ref() {
                    info!("  {} root keys: first {:?} mid {:?} last {:?}", c.name, k.first(), k.get(k.len() / 2), k.last());
                }
                info!("clip {} (root rot track: {}): root bone yaw {:.1} pitch {:.1} roll {:.1} deg, mover yaw {:.1}", c.name, c.rot[0].as_ref().map_or("none".to_string(), |k| format!("{} keys", k.len())), y.to_degrees(), x.to_degrees(), z.to_degrees(), mv.to_degrees());
            }
        }
        info!(
            "locomotion: {}",
            clips.iter().map(|c| format!("{} {:.2}s {:.1} m/s", c.name, c.duration, c.speed)).collect::<Vec<_>>().join(", ")
        );
        let extra: HashMap<String, Clip> = anims
            .iter()
            .filter(|a| !["idle", "walk", "run", "sprint"].contains(&a.name.as_str()))
            .map(|a| {
                let mut c = Clip::new(a, joint_of_id, joints);
                // only the standing idles: strafes and turns angle their root on purpose
                if a.name.starts_with("idle") {
                    align_root(&mut c, gait_yaw);
                }
                (a.name.clone(), c)
            })
            .collect();
        // turn clips per gait, e.g. walk_turn_l, walk_turn_l2, walk_turn_r3 (not the 180s)
        let mut turns: Vec<Vec<(f32, Clip)>> = (0..4).map(|_| Vec::new()).collect();
        for a in anims {
            let n = a.name.as_str();
            let tier = if n.starts_with("walk_turn_") {
                1
            } else if n.starts_with("run_turn_") {
                2
            } else if n.starts_with("sprint_turn_") {
                3
            } else {
                continue;
            };
            if n.contains("180") {
                continue;
            }
            let c = Clip::new(a, joint_of_id, joints);
            let (_, rot) = c.mover_at(c.duration);
            let rate = rot.to_euler(EulerRot::YXZ).0 / c.duration.max(0.1);
            turns[tier].push((rate, c));
        }
        for t in turns.iter_mut() {
            t.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        }
        info!(
            "turn clips (deg/s): {}",
            turns.iter().flatten().map(|(r, c)| format!("{} {:.0}", c.name, r.to_degrees())).collect::<Vec<_>>().join(", ")
        );
        Some(Locomotion { clips, extra, turns })
    }

    /// Which two clips to blend at `speed`, and how much of the second.
    pub fn blend_at(&self, speed: f32) -> (usize, usize, f32) {
        for i in 0..self.clips.len() - 1 {
            let (a, b) = (self.clips[i].speed, self.clips[i + 1].speed);
            if speed <= b {
                return (i, i + 1, ((speed - a) / (b - a).max(1e-3)).clamp(0.0, 1.0));
            }
        }
        let last = self.clips.len() - 1;
        (last, last, 0.0)
    }
}

/// Pose every joint: rest where no clip animates it, else a blend of the two clips.
pub fn pose(
    loco: &Locomotion,
    speed: f32,
    phase: f32,
    idle_phase: f32,
    rest: &[Transform],
) -> Vec<(Quat, Vec3)> {
    let (a, b, t) = loco.blend_at(speed);
    let (ca, cb) = (&loco.clips[a], &loco.clips[b]);
    let pa = if a == 0 { idle_phase } else { phase };
    let pb = if b == 0 { idle_phase } else { phase };
    rest.iter()
        .enumerate()
        .map(|(j, r)| {
            let qa = ca.rot(j, pa).unwrap_or(r.rotation);
            let qb = cb.rot(j, pb).unwrap_or(r.rotation);
            let va = ca.pos(j, pa).unwrap_or(r.translation);
            let vb = cb.pos(j, pb).unwrap_or(r.translation);
            (qa.slerp(qb, t), va.lerp(vb, t))
        })
        .collect()
}

/// The fastest the ped can turn at this speed: the sharpest turn clip of the gait(s) in use.
pub fn max_turn_rate(loco: &Locomotion, speed: f32) -> f32 {
    let (a, b, t) = loco.blend_at(speed);
    let m = |i: usize| loco.turns.get(i).map_or(0.0, |v| v.iter().map(|(r, _)| r.abs()).fold(0.0, f32::max));
    m(a) * (1.0 - t) + m(b) * t
}

/// One gait's pose with its turn lean: blend the two clips whose turn rates bracket `rate`
/// (the straight clip counts as rate 0).
fn gait_pose(loco: &Locomotion, tier: usize, rate: f32, phase: f32, rest: &[Transform]) -> Vec<(Quat, Vec3)> {
    let mut set: Vec<(f32, &Clip)> = loco.turns.get(tier).map_or(Vec::new(), |v| v.iter().map(|(r, c)| (*r, c)).collect());
    set.push((0.0, &loco.clips[tier]));
    set.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let rate = rate.clamp(set[0].0, set[set.len() - 1].0);
    let i = set.iter().rposition(|(r, _)| *r <= rate).unwrap_or(0).min(set.len().saturating_sub(2));
    let (ra, ca) = set[i];
    let (rb, cb) = set[(i + 1).min(set.len() - 1)];
    let t = if rb > ra { ((rate - ra) / (rb - ra)).clamp(0.0, 1.0) } else { 0.0 };
    rest.iter()
        .enumerate()
        .map(|(j, r)| {
            let qa = ca.rot(j, phase).unwrap_or(r.rotation);
            let qb = cb.rot(j, phase).unwrap_or(r.rotation);
            let va = ca.pos(j, phase).unwrap_or(r.translation);
            let vb = cb.pos(j, phase).unwrap_or(r.translation);
            (qa.slerp(qb, t), va.lerp(vb, t))
        })
        .collect()
}

fn pose_one(c: &Clip, phase: f32, rest: &[Transform]) -> Vec<(Quat, Vec3)> {
    rest.iter().enumerate().map(|(j, r)| (c.rot(j, phase).unwrap_or(r.rotation), c.pos(j, phase).unwrap_or(r.translation))).collect()
}

/// `pose`, plus the turn lean from the game's turn clips at yaw rate `rate` (rad/s, + = left).
pub fn pose_lean(loco: &Locomotion, speed: f32, phase: f32, idle_phase: f32, rest: &[Transform], rate: f32) -> Vec<(Quat, Vec3)> {
    let (a, b, t) = loco.blend_at(speed);
    let gp = |i: usize| if i == 0 { pose_one(&loco.clips[0], idle_phase, rest) } else { gait_pose(loco, i, rate, phase, rest) };
    let (pa, pb) = (gp(a), gp(b));
    pa.into_iter().zip(pb).map(|((qa, va), (qb, vb))| (qa.slerp(qb, t), va.lerp(vb, t))).collect()
}

/// Ground speed at IV's move blend ratio (0 still, 1 walk, 2 run, 3 sprint): IV blends the gait
/// clips by the ratio, so his root motion is the clips' speeds blended the same way.
pub fn mbr_speed(loco: &Locomotion, mbr: f32) -> f32 {
    let m = mbr.clamp(0.0, (loco.clips.len() - 1) as f32);
    let i = (m.floor() as usize).min(loco.clips.len() - 2);
    let f = m - i as f32;
    loco.clips[i].speed * (1.0 - f) + loco.clips[i + 1].speed * f
}

/// The move blend ratio whose blended clips move at `speed` (the inverse of `mbr_speed`).
pub fn speed_mbr(loco: &Locomotion, speed: f32) -> f32 {
    let (a, b, t) = loco.blend_at(speed);
    if a == b {
        a as f32
    } else {
        a as f32 + t
    }
}

/// How long one cycle lasts at this blend (so phase advances at the right rate).
pub fn cycle_duration(loco: &Locomotion, speed: f32) -> f32 {
    let (a, b, t) = loco.blend_at(speed);
    let (a, b) = (a.max(1), b.max(1)); // idle doesn't set the stepping rate
    loco.clips[a].duration * (1.0 - t) + loco.clips[b].duration * t
}
