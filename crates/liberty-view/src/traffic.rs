//! Traffic: cars driving the game's own road network (`paths.img`, decoded in `rage::paths`).
//!
//! First version: every car link is stored both ways in the file (47,928 of 47,928 have a reverse
//! link), so roads are treated as two-way and cars keep right of the node line. Cars are moved
//! kinematically along links (no physics) until the player takes one over with F, when the normal
//! car physics picks it up at the speed it was going.

use std::path::Path;

use anyhow::{Context, Result};
use bevy::prelude::*;
use bevy::render::mesh::skinning::SkinnedMeshInverseBindposes;
use rage::{Img, Key};

use crate::assets::to_bevy;
use crate::collision;
use crate::stream::City;
use crate::vehicle::{self, Car, Garage};
use crate::Fly;

/// How far right of the node line a car drives (half a lane-and-a-bit).
/// IV's light clock (exe 0xd210e0 / 0xd21140, the same pair the Xbox build has; cross-checked
/// with ed-slopper/large-steal-car-4's reading): two sets of lights on one 40 s cycle from the
/// game's start (25 s in some zones: STAND-IN, which zones isn't read, the 1 Oct junction ran 40):
/// - the first set: green 0-25 s, amber to 27.5 s, red to 40 s;
/// - the second: red to 27.5 s, green to 37.5 s, amber to 40 s.
/// Our link phases (paths.img lanes bits 6-7) are the Xbox's bits the other way round: phase 2 is
/// the first set (757 of its 810 links run along x), phase 1 the second (852 of 882 along y).
/// Measured on 1 Oct in the real game: green 26.2 s, amber 2.3 s, the crossing road back within a
/// second of red (the 57 s cycle we had assumed both ways was the first set seen once).
const CYCLE: f32 = 40.0;

/// The two sets' signal at `time` (s): 0 green, 1 amber, 2 red.
fn set_state(first: bool, time: f32) -> u8 {
    let t = time.rem_euclid(CYCLE);
    if first {
        if t < 25.0 { 0 } else if t < 27.5 { 1 } else { 2 }
    } else if t < 27.5 {
        2
    } else if t < 37.5 {
        0
    } else {
        1
    }
}

/// Is a traffic-light phase (1 or 2; 0 = no light) green at `time` seconds? Amber counts as stop.
pub fn light_green(phase: u8, time: f32) -> bool {
    phase == 0 || light_state(phase, time) == 0
}

/// What a phase's signal shows at `time`: 0 green, 1 amber, 2 red.
pub fn light_state(phase: u8, time: f32) -> u8 {
    set_state(phase == 2, time)
}

/// What a pedestrian at a crossing sees, walking along (dx, dy) in GTA space: 0 walk, 1 flashing,
/// 2 don't walk. IV (exe 0xd211a0): mostly along x, the first set's pattern, else the second's;
/// so peds walk beside the green traffic, across the road that's held at red.
pub fn walk_signal(dx: f32, dy: f32, time: f32) -> u8 {
    set_state(dx.abs() > dy.abs(), time)
}

/// Distance from `p` to the segment a-b, on the ground plane.
fn seg_dist(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let (p, a, b) = (p.with_y(0.0), a.with_y(0.0), b.with_y(0.0));
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// `Roads::next` when a car has reached the end of a one-way road with no legal way on.
const NO_WAY: u32 = u32::MAX;
/// The first lane's middle on a two-way road, metres right of the node line (half a lane width).
const LANE: f32 = rage::paths::LANE_WIDTH / 2.0;
const SPAWN_MIN: f32 = 45.0;
const SPAWN_MAX: f32 = 130.0;
const DESPAWN: f32 = 170.0;

/// Car-only road graph, positions in Bevy space.
#[derive(Resource)]
pub struct Roads {
    pub pos: Vec<Vec3>,
    pub adj: Vec<Vec<u32>>,
    /// Each link's u16 flags, parallel to `adj` (not decoded).
    pub link_flags: Vec<Vec<u16>>,
    /// Each link's lanes (its own way, the other way), parallel to `adj`.
    pub lanes: Vec<Vec<(u8, u8)>>,
    /// Each link's traffic-light phase (0 none, 1, 2: a light at the link's end), parallel to `adj`.
    pub lights: Vec<Vec<u8>>,
    /// The pedestrian road crossings: paths.img's "ped" nodes are all of one kind (type 0xB in
    /// flags bits 4-7), a node on each kerb linked to the one across the road (2,522 nodes). IV's
    /// wandering peds look for one ahead of them and cross node to node (exe: the wander task
    /// 0xcb1750 -> 0xcbb1c0). Positions in Bevy space.
    pub cross_pos: Vec<Vec3>,
    /// Per crossing node: the nodes across the road from it, each with the traffic-light phase of
    /// the road it crosses (0: no light there; STAND-IN for IV's "a traffic light within 10 m of
    /// the node": a lit road link across it within 20 m). The ped signal itself goes by the
    /// crossing's axis (`walk_signal`), as IV's does.
    pub cross_links: Vec<Vec<(u32, u8)>>,
    /// Road models traffic picks from (plain cars, no emergency/odd vehicles).
    models: Vec<String>,
    count: usize,
    /// The dev panel's Traffic switch. Off: no new cars and the ones on the road go. Starts off
    /// with `LIBERTY_TRAFFIC=0`.
    pub on: bool,
    timer: f32,
    rng: u64,
}

impl Roads {
    pub fn load(root: &Path, key: &Key, garage: &Garage) -> Result<Roads> {
        let img = Img::open(&root.join("pc/data/cdimages/paths.img"), key)?;
        let mut areas = Vec::with_capacity(64);
        for i in 0..64u32 {
            let name = format!("nodes{i}.nod");
            let e = img.find(&name).with_context(|| format!("{name} not in paths.img"))?;
            areas.push(rage::paths::read_nod(&img.read(e)?, i)?);
        }
        // global index per (area, node) for car nodes
        let mut base = Vec::with_capacity(64);
        let mut pos = Vec::new();
        for a in &areas {
            base.push(pos.len() as u32);
            for n in a.nodes.iter().take(a.car_nodes as usize) {
                pos.push(Vec3::from(to_bevy(n.pos)));
            }
        }
        let mut adj = vec![Vec::new(); pos.len()];
        let mut link_flags = vec![Vec::new(); pos.len()];
        let mut lanes = vec![Vec::new(); pos.len()];
        let mut lights = vec![Vec::new(); pos.len()];
        for (ai, a) in areas.iter().enumerate() {
            for (ni, n) in a.nodes.iter().enumerate().take(a.car_nodes as usize) {
                if n.water() || n.boat() {
                    continue;
                }
                let from = base[ai] + ni as u32;
                for l in &a.links[n.first_link as usize..(n.first_link + n.link_count as u32) as usize] {
                    let Some(t) = areas.get(l.area as usize) else { continue };
                    if (l.node as u32) < t.car_nodes && !t.nodes[l.node as usize].water() && !t.nodes[l.node as usize].boat() {
                        adj[from as usize].push(base[l.area as usize] + l.node as u32);
                        link_flags[from as usize].push(l.flags);
                        lanes[from as usize].push((l.lanes_with(), l.lanes_against()));
                        lights[from as usize].push(l.light_phase());
                    }
                }
            }
        }
        // the crossings: every ped node, linked to the ped nodes across the road
        let mut cross_base = Vec::with_capacity(64);
        let mut cross_pos = Vec::new();
        for a in &areas {
            cross_base.push(cross_pos.len() as u32);
            for n in a.nodes.iter().skip(a.car_nodes as usize) {
                cross_pos.push(Vec3::from(to_bevy(n.pos)));
            }
        }
        // lit links, for which light holds the traffic a crossing walks across
        let lit: Vec<(Vec3, Vec3, u8)> = (0..pos.len()).flat_map(|f| adj[f].iter().zip(&lights[f]).filter(|(_, p)| **p != 0).map(move |(t, p)| (f, *t as usize, *p))).map(|(f, t, p)| (pos[f], pos[t], p)).collect();
        let mut cross_links = vec![Vec::new(); cross_pos.len()];
        for (ai, a) in areas.iter().enumerate() {
            for (ni, n) in a.nodes.iter().enumerate().skip(a.car_nodes as usize) {
                let from = cross_base[ai] + (ni - a.car_nodes as usize) as u32;
                for l in &a.links[n.first_link as usize..(n.first_link + n.link_count as u32) as usize] {
                    let Some(t) = areas.get(l.area as usize) else { continue };
                    if (l.node as u32) < t.car_nodes || l.node as usize >= t.nodes.len() {
                        continue;
                    }
                    let to = cross_base[l.area as usize] + l.node as u32 - t.car_nodes;
                    let (p, q) = (cross_pos[from as usize], cross_pos[to as usize]);
                    let (mid, dir) = ((p + q) * 0.5, (q - p).with_y(0.0).normalize_or_zero());
                    // the lit approach nearest the crossing that runs across it (the road it crosses)
                    let phase = lit
                        .iter()
                        .filter(|(a, b, _)| (*b - *a).with_y(0.0).normalize_or_zero().dot(dir).abs() < 0.5)
                        .map(|(a, b, ph)| (seg_dist(mid, *a, *b), *ph))
                        .filter(|(d, _)| *d < 20.0)
                        .min_by(|x, y| x.0.total_cmp(&y.0))
                        .map_or(0, |x| x.1);
                    cross_links[from as usize].push((to, phase));
                }
            }
        }
        info!("roads: {} crossing nodes, {} crossings at a light", cross_pos.len(), cross_links.iter().flatten().filter(|l| l.1 != 0).count() / 2);
        let skip = ["ambulance", "police", "police2", "polpatriot", "noose", "nstockade", "firetruk", "fbi", "pstockade", "bus", "cabby", "taxi", "taxi2", "stockade", "trash", "flatbed", "packer", "phantom", "hellfury", "zombieb", "biff", "mule", "yankee", "benson", "boxville", "steed", "airtug", "ripley", "forklift", "coach"];
        // LIBERTY_TRAFFIC=0 starts with traffic off (Roads.on); switched on later it is the default count
        let count = match std::env::var("LIBERTY_TRAFFIC").ok().and_then(|v| v.parse().ok()) {
            Some(0) | None => 14,
            Some(n) => n,
        };
        let on = std::env::var("LIBERTY_TRAFFIC").ok().and_then(|v| v.parse::<usize>().ok()) != Some(0);
        // IV's own car groups (common/data/cargrp.dat): a car that's in no group never drives in
        // traffic, and the ONLY_IN_NATIVE_ZONE ones (the airport's security Perennial / Feroci
        // with the amber beacon, the ice-cream van, the airport tug...) only in their own zone,
        // which we don't place yet. STAND-IN: one pool for the whole city; IV picks per zone
        // and time of day from popcycle.dat.
        let groups = std::fs::read_to_string(root.join("common/data/cargrp.dat")).unwrap_or_default();
        let mut in_group = std::collections::HashSet::new();
        let mut native = std::collections::HashSet::new();
        for line in groups.lines().filter(|l| !l.trim_start().starts_with('#')) {
            let (cars, group) = line.split_once('#').unwrap_or((line, ""));
            for car in cars.split(',').map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty()) {
                if group.contains("ONLY_IN_NATIVE_ZONE") {
                    native.insert(car.clone());
                }
                in_group.insert(car);
            }
        }
        let models: Vec<String> = garage
            .models
            .iter()
            .filter(|m| !skip.contains(&m.as_str()) && (in_group.is_empty() || (in_group.contains(*m) && !native.contains(*m))))
            .cloned()
            .collect();
        info!("roads: {} car nodes, {} links; traffic {count} cars from {} models", pos.len(), adj.iter().map(Vec::len).sum::<usize>(), models.len());
        let one_way = lanes.iter().flatten().filter(|l| l.0 > 0 && l.1 == 0).count();
        info!("roads: {one_way} one-way links, {} with more than one lane a side", lanes.iter().flatten().filter(|l| l.0 > 1).count());
        info!("roads: {} links end at a traffic light", lights.iter().flatten().filter(|p| **p != 0).count());
        Ok(Roads { pos, adj, link_flags, lanes, lights, cross_pos, cross_links, models, count, on, timer: 0.0, rng: 0x9E37_79B9_7F4A_7C15 })
    }

    fn rand(&mut self) -> f32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Which phase a signal head runs on: the lit link that ends nearest to it and drives toward
    /// its face (`facing` = the way the head points, level). 0 if no such link within 45 m.
    pub fn signal_phase(&self, at: Vec3, facing: Vec3) -> u8 {
        let mut best = (f32::MIN, 0u8);
        for (from, links) in self.adj.iter().enumerate() {
            for (k, &to) in links.iter().enumerate() {
                let phase = self.lights[from][k];
                if phase == 0 {
                    continue;
                }
                let end = self.pos[to as usize];
                let d = (end - at).with_y(0.0).length();
                if d > 45.0 || (end.y - at.y).abs() > 12.0 {
                    continue;
                }
                let along = (end - self.pos[from]).with_y(0.0).normalize_or_zero();
                // traffic on this link drives against the way the head faces
                let toward = -along.dot(facing);
                let score = toward - d / 45.0;
                if toward > 0.6 && score > best.0 {
                    best = (score, phase);
                }
            }
        }
        best.1
    }

    /// The traffic-light phase at the end of a link (0 = no light).
    pub fn link_light(&self, from: u32, to: u32) -> u8 {
        self.adj[from as usize].iter().position(|&n| n == to).map_or(0, |k| self.lights[from as usize][k])
    }

    /// A link's lanes (its own way, the other way).
    pub fn link_lanes(&self, from: u32, to: u32) -> (u8, u8) {
        self.adj[from as usize].iter().position(|&n| n == to).map_or((1, 1), |k| self.lanes[from as usize][k])
    }

    /// A random link whose start is in the ring around `at`.
    fn pick_link(&mut self, at: Vec3) -> Option<(u32, u32)> {
        let near: Vec<u32> = (0..self.pos.len() as u32)
            .filter(|&i| {
                let d = (self.pos[i as usize] - at).with_y(0.0).length();
                // somewhere traffic may leave from: a link with a lane going its way
                (SPAWN_MIN..SPAWN_MAX).contains(&d) && (self.pos[i as usize].y - at.y).abs() < 30.0 && self.lanes[i as usize].iter().any(|l| l.0 > 0)
            })
            .collect();
        if near.is_empty() {
            return None;
        }
        let a = near[(self.rand() * near.len() as f32) as usize % near.len()];
        let r = self.rand();
        let n: Vec<u32> = self.adj[a as usize].iter().zip(&self.lanes[a as usize]).filter(|(_, l)| l.0 > 0).map(|(n, _)| *n).collect();
        let b = n[(r * n.len() as f32) as usize % n.len()];
        Some((a, b))
    }

    /// Next node after arriving at `to` from `from`: mostly straight on, sometimes a turn,
    /// back the way we came only at dead ends.
    fn next(&mut self, from: u32, to: u32) -> u32 {
        let d_in = (self.pos[to as usize] - self.pos[from as usize]).with_y(0.0).normalize_or_zero();
        let opts: Vec<(u32, f32)> = self.adj[to as usize]
            .iter()
            .zip(&self.lanes[to as usize])
            // not back the way he came, and not the wrong way up a one-way street
            .filter(|&(&n, l)| n != from && l.0 > 0)
            .map(|(&n, _)| (n, (self.pos[n as usize] - self.pos[to as usize]).with_y(0.0).normalize_or_zero().dot(d_in)))
            .filter(|&(_, c)| c > -0.3)
            .collect();
        if opts.is_empty() {
            // no way on within a gentle turn: any legal exit (a sharp turn) before turning back,
            // and back only if the road he came on has a lane that way
            let legal: Vec<u32> = self.adj[to as usize].iter().zip(&self.lanes[to as usize]).filter(|&(&n, l)| n != from && l.0 > 0).map(|(&n, _)| n).collect();
            if !legal.is_empty() {
                return legal[(self.rand() * legal.len() as f32) as usize % legal.len()];
            }
            // a true dead end: back along the road he came on if it has a lane that way,
            // otherwise there's nowhere legal to go (NO_WAY: the car is taken off the road)
            return if self.link_lanes(to, from).0 > 0 { from } else { NO_WAY };
        }
        // weight: straight 1.0, turns ~0.35
        let w: Vec<f32> = opts.iter().map(|&(_, c)| if c > 0.8 { 1.0 } else { 0.35 }).collect();
        let mut r = self.rand() * w.iter().sum::<f32>();
        for (i, wi) in w.iter().enumerate() {
            if r < *wi {
                return opts[i].0;
            }
            r -= wi;
        }
        opts[0].0
    }
}

#[derive(Component)]
pub struct Traffic {
    from: u32,
    to: u32,
    /// Metres along the current link.
    along: f32,
    speed: f32,
    cruise: f32,
    yaw: f32,
    ride: f32,
    /// Which lane he's in (0 = innermost).
    lane: u8,
    /// The node he came from before `from` and his lane on that stretch (for the corner he's
    /// still coming out of; NO_WAY when he spawned on this stretch).
    prev: u32,
    prev_lane: u8,
    /// Where he goes after `to`, decided as he joins each stretch so he can slow for a turn.
    next: Option<u32>,
    /// Slowing for (or in) a turn at the end of this stretch.
    turning: bool,
    /// Waiting for a car crossing in front of him.
    crossing: bool,
    /// Stopped (or stopping) for a red light at the end of his link.
    at_red: bool,
    /// Seconds he's been held up by someone on foot (he sounds the horn after a while).
    held_up: f32,
    /// Frightened by gunfire: seconds left of stopping dead (ducking), then of driving off fast.
    duck: f32,
    panic: f32,
}

impl Traffic {
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// For tests: what the driver is doing.
    pub fn state(&self) -> String {
        let doing = if self.duck > 0.0 {
            "ducking"
        } else if self.panic > 0.0 {
            "speeding off"
        } else if self.at_red {
            "at a red light"
        } else if self.crossing {
            "waiting for a crossing car"
        } else if self.turning {
            "slowing for a turn"
        } else if self.held_up > 0.0 {
            "waiting for someone on foot"
        } else {
            "driving"
        };
        format!("{doing}, lane {}, {:.0} km/h", self.lane, self.speed * 3.6)
    }
}

/// The closest right-hand lane point to `p` (within `max` metres, similar height), with its
/// driving direction — the one of the two that best matches `prefer`. For spawning on roads.
pub fn nearest_lane(roads: &Roads, p: Vec3, prefer: Vec3, max: f32) -> Option<(Vec3, Vec3)> {
    let mut best: Option<(f32, Vec3, Vec3)> = None;
    for (a, links) in roads.adj.iter().enumerate() {
        let pa = roads.pos[a];
        if (pa - p).with_y(0.0).length() > max + 60.0 || (pa.y - p.y).abs() > 8.0 {
            continue;
        }
        for &b in links {
            let pb = roads.pos[b as usize];
            let seg = (pb - pa).with_y(0.0);
            let len = seg.length();
            if len < 0.5 {
                continue;
            }
            let dir = seg / len;
            let dir = if dir.dot(prefer) >= 0.0 { dir } else { -dir };
            let right = Vec3::new(-dir.z, 0.0, dir.x);
            let t = ((p - pa).with_y(0.0).dot(seg / len) / len).clamp(0.1, 0.9);
            let q = pa.lerp(pb, t) + right * LANE;
            let d = (q - p).with_y(0.0).length();
            if d < max && best.is_none_or(|b| d < b.0) {
                best = Some((d, q, dir));
            }
        }
    }
    best.map(|(_, q, d)| (q, d))
}

/// A point along a link in one of its lanes (IV's own lane positions: `rage::paths::lane_offset`),
/// and the driving direction.
fn lane_point(roads: &Roads, from: u32, to: u32, along: f32, lane: u8) -> (Vec3, Vec3) {
    let (a, b) = (roads.pos[from as usize], roads.pos[to as usize]);
    let dir = (b - a).with_y(0.0).normalize_or(Vec3::NEG_Z);
    let right = Vec3::new(-dir.z, 0.0, dir.x);
    let t = (along / (b - a).length().max(0.01)).clamp(0.0, 1.0);
    let (with, against) = roads.link_lanes(from, to);
    (a.lerp(b, t) + right * rage::paths::lane_offset(with, against, lane), dir)
}

/// The corner at node `at`, from lane `lane_in` on the stretch in from `prev` to the matching lane
/// on the stretch out to `next`: a quadratic Bezier that leaves the incoming lane `r` m (along the
/// road) before the node and joins the outgoing one `r` m after it, bent through where the two lane
/// lines cross. `r` is the tangent length of a ~7 m turning circle for the turn angle (longer where
/// the lane position jumps), at most half of either stretch so corners never overlap (ours, not
/// IV's). `u` is metres past the node (negative before it). Point and driving direction, or None
/// when he's not in a corner (straight on, too far from the node, or a U-turn).
fn corner(roads: &Roads, prev: u32, at: u32, next: u32, lane_in: u8, u: f32) -> Option<(Vec3, Vec3)> {
    let (a, n, b) = (roads.pos[prev as usize], roads.pos[at as usize], roads.pos[next as usize]);
    let (len_in, len_out) = ((n - a).length(), (b - n).length());
    let d_in = (n - a).with_y(0.0).normalize_or_zero();
    let d_out = (b - n).with_y(0.0).normalize_or_zero();
    let cos = d_in.dot(d_out);
    if cos < -0.7 {
        return None;
    }
    let lane_out = lane_in.min(roads.link_lanes(at, next).0.saturating_sub(1));
    let p_in = lane_point(roads, prev, at, len_in, lane_in).0;
    let p_out = lane_point(roads, at, next, 0.0, lane_out).0;
    let jump = (p_out - p_in).with_y(0.0).length();
    let r = (7.0 * ((1.0 - cos) / (1.0 + cos)).max(0.0).sqrt()).max(jump * 4.0).min(0.5 * len_in.min(len_out));
    if r < 0.3 || u.abs() >= r {
        return None;
    }
    let p0 = lane_point(roads, prev, at, len_in - r, lane_in).0;
    let p2 = lane_point(roads, at, next, r, lane_out).0;
    // where the lane lines cross (on the ground plane), else halfway between their ends
    let mid = (p_in + p_out) * 0.5;
    let cross = d_in.x * d_out.z - d_in.z * d_out.x;
    let p1 = if cross.abs() > 0.05 {
        let w = p_out - p_in;
        let c = p_in + d_in * ((w.x * d_out.z - w.z * d_out.x) / cross);
        if (c - mid).with_y(0.0).length() < r { c.with_y(mid.y) } else { mid }
    } else {
        mid
    };
    let k = (u + r) / (2.0 * r);
    let p = p0 * (1.0 - k) * (1.0 - k) + p1 * 2.0 * (1.0 - k) * k + p2 * k * k;
    let tangent = (p1 - p0) * (1.0 - k) + (p2 - p1) * k;
    Some((p, tangent.with_y(0.0).normalize_or(d_out)))
}

/// Ram test: the direction the car comes from, for Niko to turn toward (read once by ped.rs).
pub static RAM_FACE: std::sync::Mutex<Option<Vec3>> = std::sync::Mutex::new(None);

/// Keep `LIBERTY_TRAFFIC` (default 14) cars on the roads around the camera.
#[allow(clippy::too_many_arguments)]
pub fn spawn_traffic(
    mut commands: Commands,
    time: Res<Time>,
    mut roads: ResMut<Roads>,
    garage: Res<Garage>,
    city: Res<City>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    cam: Query<&Transform, With<Fly>>,
    traffic: Query<(Entity, &Transform), With<Traffic>>,
    loose: Query<(Entity, &Transform), With<vehicle::Loose>>,
    fly: Query<&Fly>,
    started: Option<Res<crate::RecordStart>>,
    players: Query<&Transform, (With<crate::ped::Player>, Without<Fly>)>,
    peds: Query<&Transform, With<crate::peds::Npc>>,
) {
    let _slow = crate::SlowGuard::new("traffic::spawn_traffic");
    let Ok(cam_t) = cam.single() else { return };
    let at = cam_t.translation;
    let driving = fly.single().ok().and_then(|f| f.driving);
    if !roads.on {
        // switched off: the cars on the road go (their lights and engine sounds are children, so they go too)
        for (e, _) in &traffic {
            if driving != Some(e) {
                commands.entity(e).despawn();
            }
        }
    }
    for (e, t) in traffic.iter().chain(loose.iter()) {
        if driving != Some(e) && (t.translation - at).with_y(0.0).length() > DESPAWN {
            commands.entity(e).despawn();
        }
    }
    // LIBERTY_RAM_TEST: once he's on foot, a car 24 m ahead of the camera comes straight at him
    // at 11 m/s (≈40 km/h) — for recording braceForImpact
    if std::env::var("LIBERTY_RAM_TEST").is_ok() && traffic.is_empty() && loose.is_empty() && driving.is_none() && started.is_some() {
        // 24 m out along the camera's view from wherever Niko stands, aimed at him
        let fwd = cam_t.forward().with_y(0.0).normalize_or(Vec3::NEG_Z);
        // wait for Niko: before he's placed, the camera is somewhere else entirely
        let Ok(niko_t) = players.single() else { return };
        let niko = niko_t.translation;
        if (niko - at).with_y(0.0).length() > 15.0 {
            return;
        }
        // LIBERTY_RAM_TARGET=ped: the car goes for the ped nearest him instead (LIBERTY_TARGET_PED
        // puts one there), once it has spawned; with LIBERTY_RAM_DIR across his view it misses Niko
        let niko = if std::env::var("LIBERTY_RAM_TARGET").is_ok_and(|v| v == "ped") {
            let Some(ped) = peds.iter().map(|t| t.translation).min_by(|a, b| a.distance(niko).total_cmp(&b.distance(niko))) else { return };
            ped
        } else {
            niko
        };
        // LIBERTY_RAM_DIR=x,z: where the car starts from, relative to Niko (Bevy axes), instead of
        // along the camera view (which varies run to run); Niko is turned to face it (RAM_FACE)
        let fwd = std::env::var("LIBERTY_RAM_DIR")
            .ok()
            .and_then(|v| {
                let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (n.len() == 2).then(|| Vec3::new(n[0], 0.0, n[1]).normalize_or(fwd))
            })
            .unwrap_or(fwd);
        // LIBERTY_RAM_SPEED=m/s (default 11; IV's recorded hit: 7.5). The car only gets a push, so it
        // starts about 2 s out (24 m at the default speed) to arrive near that speed
        let speed = std::env::var("LIBERTY_RAM_SPEED").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(11.0);
        let dist = (speed * 2.2).clamp(10.0, 24.0);
        // LIBERTY_RAM_BRAKE=m: where the driver brakes, relative to Niko's spot (default 5 m past him;
        // IV's recorded hit braked about 1 m before: -1)
        let brake_at = std::env::var("LIBERTY_RAM_BRAKE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(5.0);
        let p = niko + fwd * dist;
        if let Some(g) = collision::ground_height(city.collision.values(), p.x, p.z, at.y + 3.0) {
            let to = (niko - p).with_y(0.0).normalize_or(-fwd);
            let yaw = (-to.x).atan2(-to.z); // facing Niko
            match vehicle::spawn_car_named(&mut commands, &garage, &mut meshes, &mut materials, &mut images, &mut bindposes, Vec3::new(p.x, g, p.z), yaw, "admiral") {
                Ok(e) => {
                    info!("ram test: car launched at {p:?} (niko {niko:?}, dir {fwd:?})");
                    if let Ok(mut f) = RAM_FACE.lock() {
                        *f = Some(fwd);
                    }
                    commands.entity(e).insert((vehicle::Loose, vehicle::Launch((niko - p).with_y(0.0).normalize_or(-fwd) * speed)));
                    // the driver stands on the brakes as he hits (24 m at 11 m/s ≈ 2.2 s)
                    commands.entity(e).insert(vehicle::BrakeAfter((dist + brake_at) / speed));
                }
                Err(err) => warn!("ram test: {err:#}"),
            }
        } else {
            info!("ram test: no ground at {p:?}");
        }
        return;
    }
    if std::env::var("LIBERTY_RAM_TEST").is_ok() {
        return; // the test wants the road to itself
    }
    roads.timer -= time.delta_secs();
    if !roads.on || roads.timer > 0.0 || traffic.iter().count() >= roads.count || roads.models.is_empty() || city.collision.is_empty() {
        return;
    }
    roads.timer = 0.25; // one car per quarter second: each spawn decodes a model
    // LIBERTY_CRASH_TEST: first car stands still across the road 22 m ahead of the camera (a T-bone target)
    if std::env::var("LIBERTY_CRASH_TEST").is_ok() && traffic.is_empty() && loose.is_empty() {
        if driving.is_none() {
            return; // place it from the chase camera, once the test car is being driven
        }
        let fwd = cam_t.forward().with_y(0.0).normalize_or(Vec3::NEG_Z);
        let p = at + fwd * 22.0;
        if let Some(g) = collision::ground_height(city.collision.values(), p.x, p.z, at.y + 2.0) {
            let yaw = fwd.x.atan2(fwd.z) + std::f32::consts::FRAC_PI_2;
            if let Ok(e) = vehicle::spawn_car_named(&mut commands, &garage, &mut meshes, &mut materials, &mut images, &mut bindposes, Vec3::new(p.x, g, p.z), yaw, "admiral") {
                commands.entity(e).insert(vehicle::Loose);
            }
        }
        return;
    }    let Some((from, to)) = roads.pick_link(at) else { return };
    let lane = (roads.rand() * roads.link_lanes(from, to).0 as f32) as u8;
    let (p, dir) = lane_point(&roads, from, to, 0.0, lane);
    // not on top of another car
    if traffic.iter().any(|(_, t)| (t.translation - p).length() < 10.0) {
        return;
    }
    let Some(g) = collision::ground_height(city.collision.values(), p.x, p.z, p.y + 3.0) else { return };
    let r = roads.rand();
    let model = roads.models[(r * roads.models.len() as f32) as usize % roads.models.len()].clone();
    let yaw = (-dir.x).atan2(-dir.z);
    match vehicle::spawn_car_named(&mut commands, &garage, &mut meshes, &mut materials, &mut images, &mut bindposes, Vec3::new(p.x, g, p.z), yaw, &model) {
        Ok(e) => {
            // IV's own free-flowing traffic (re/giveway2.py, 4,606 passes): middle half 8.2 to
            // 13.2 m/s, median 10
            let cruise = 7.0 + roads.rand() * 6.0;
            commands.entity(e).insert(Traffic { from, to, along: 0.0, speed: cruise * 0.6, cruise, yaw, ride: -1.0, lane, prev: NO_WAY, prev_lane: lane, next: None, turning: false, crossing: false, at_red: false, held_up: 0.0, duck: 0.0, panic: 0.0 });
        }
        Err(err) => warn!("traffic: {model}: {err:#}"),
    }
}

/// Drive every traffic car along its link: follow the lane, slow for whatever is ahead (another
/// car, the player's, or anyone on foot in the way), pick the next link at the end. A driver
/// held up by someone on foot sounds his horn; gunfire nearby makes him duck or drive off fast.
/// (The distances and times here are ours, not measured from IV.)
#[allow(clippy::too_many_arguments)]
pub fn move_traffic(
    mut commands: Commands,
    time: Res<Time>,
    mut roads: ResMut<Roads>,
    city: Res<City>,
    fly: Query<&Fly>,
    mut cars: Query<(Entity, &mut Transform, &mut Car, Option<&mut Traffic>)>,
    npcs: Query<&GlobalTransform, With<crate::peds::Npc>>,
    player: Query<&GlobalTransform, With<crate::ped::Player>>,
    armed: Option<Res<crate::weapon::Armed>>,
    mut hits: EventReader<crate::weapon::BulletHit>,
    mut honk: EventWriter<crate::sound::Honk>,
    mut fired: Local<u32>,
    mut debug_at: Local<f32>,
) {
    let _slow = crate::SlowGuard::new("traffic::move_traffic");
    let dt = time.delta_secs().min(0.1);
    let driving = fly.single().ok().and_then(|f| f.driving);
    // everyone on foot (Niko too, unless he's in a car)
    let niko = player.single().ok().map(|p| p.translation());
    let mut people: Vec<Vec3> = npcs.iter().map(|g| g.translation()).collect();
    if let (Some(p), None) = (niko, driving) {
        people.push(p);
    }
    let gunfire = armed.as_ref().is_some_and(|a| a.shots != *fired);
    if let Some(a) = &armed {
        *fired = a.shots;
    }
    let shot_at: Vec<Entity> = hits.read().filter_map(|h| h.entity).collect();
    // LIBERTY_TRAFFIC_DEBUG: twice a second, what every driver is doing
    let debug = std::env::var("LIBERTY_TRAFFIC_DEBUG").is_ok() && time.elapsed_secs() - *debug_at >= 0.5;
    if debug {
        *debug_at = time.elapsed_secs();
    }
    // everything a traffic car must not drive into
    let obstacles: Vec<(Entity, Vec3)> = cars.iter().map(|(e, t, _, _)| (e, t.translation)).collect();
    // heading of every traffic car that is moving (a parked car, or one standing at its own red
    // light, isn't crossing anyone's path)
    let headings: Vec<Option<Vec3>> = cars.iter().map(|(_, t, _, tr)| tr.filter(|tr| tr.speed > 1.0).map(|_| t.rotation * Vec3::NEG_Z)).collect();
    for (e, mut t, mut car, tr) in &mut cars {
        let Some(mut tr) = tr else { continue };
        if driving == Some(e) {
            // the player took it: hand over to the physics at the current speed
            commands.entity(e).remove::<Traffic>();
            continue;
        }
        if tr.ride < 0.0 {
            tr.ride = car.ride_height();
        }
        if debug {
            // position and heading in GTA axes, for re/lanes_ours.py (the same check as on IV's traffic)
            let f = t.rotation * Vec3::NEG_Z;
            let phase = roads.link_light(tr.from, tr.to);
            let light = if phase == 0 { "no light".to_string() } else { format!("light phase {phase} {} {:.1} m ahead", if light_green(phase, time.elapsed_secs()) { "green" } else { "red" }, (roads.pos[tr.to as usize] - roads.pos[tr.from as usize]).length() - tr.along) };
            info!("car {e} ({}): {}, {:.0} m from Niko, at {:.2} {:.2} {:.2} heading {:.3} {:.3}, {light}", car.model, tr.state(), niko.map_or(0.0, |n| n.distance(t.translation)), t.translation.x, -t.translation.z, t.translation.y, f.x, -f.z);
        }
        // gunfire within earshot, or a bullet in his own car: with the shooter close in front he
        // ducks (stops dead) for a few seconds; either way he then drives off fast
        if let Some(from) = niko.filter(|n| (gunfire && n.distance(t.translation) < 60.0) || shot_at.contains(&e)) {
            let to = (from - t.translation).with_y(0.0);
            let ahead = to.normalize_or_zero().dot(t.rotation * Vec3::NEG_Z);
            if tr.panic <= 0.0 && to.length() < 30.0 && ahead > 0.4 {
                tr.duck = 3.0 + roads.rand() * 3.0;
            }
            tr.panic = 10.0 + roads.rand() * 5.0;
        }
        // gap to the nearest thing ahead in our lane
        let fwd = t.rotation * Vec3::NEG_Z;
        let gap = obstacles
            .iter()
            .filter(|(o, _)| *o != e)
            .map(|(_, p)| *p - t.translation)
            .filter(|d| {
                let ahead = d.dot(fwd);
                ahead > 0.0 && (d - fwd * ahead).with_y(0.0).length() < 2.2 && d.y.abs() < 4.0
            })
            .map(|d| d.dot(fwd))
            .fold(f32::MAX, f32::min);
        // someone on foot in his lane ahead (a narrower corridor than for cars: people on the
        // pavement beside the road don't count)
        let person = people
            .iter()
            .map(|p| *p - t.translation)
            .filter(|d| {
                let ahead = d.dot(fwd);
                ahead > 1.0 && (d - fwd * ahead).with_y(0.0).length() < 1.5 && d.y.abs() < 3.0
            })
            .map(|d| d.dot(fwd))
            .fold(f32::MAX, f32::min);
        let cruise = if tr.panic > 0.0 { tr.cruise * 1.9 } else { tr.cruise };
        let mut want = if gap < 7.0 { 0.0 } else if gap < 20.0 { cruise * (gap - 7.0) / 13.0 } else { cruise };
        // a car on a crossing path in front of his nose: he waits. IV's own cars do (re/crossing.py:
        // with a crossing car inside 8 m ahead, both are moving in only 2% of 514 looks; the one
        // behind is stopped 77-96% of the time). When two cars each have the other in front, one
        // of them goes (ours: the tie-break isn't from IV).
        tr.crossing = false;
        if tr.panic <= 0.0 {
            let cross = obstacles
                .iter()
                .zip(&headings)
                .filter_map(|((o, p), h)| h.map(|h| (o, p, h)))
                .filter(|(o, _, h)| **o != e && h.dot(fwd).abs() < 0.5)
                .filter_map(|(o, p, h)| {
                    let d = (*p - t.translation).with_y(0.0);
                    let dist = d.length();
                    let in_front = dist > 0.5 && dist < 15.0 && (p.y - t.translation.y).abs() < 3.0 && d.dot(fwd) / dist > 0.7;
                    let mutual = (-d).dot(h) / dist.max(0.01) > 0.7;
                    (in_front && !(mutual && e.index() < o.index())).then_some(dist)
                })
                .fold(f32::MAX, f32::min);
            if cross < 15.0 {
                tr.crossing = true;
                want = want.min(if cross < 8.0 { 0.0 } else { cruise * (cross - 8.0) / 7.0 });
            }
        }
        // stop 5 m short of a person, start slowing 18 m out
        // a red light at the end of this link: stop 1.5 m short of the line. Too close to stop
        // when it changes (inside 2.5 m and still rolling), he carries on through. A frightened
        // driver runs it.
        let mut phase = roads.link_light(tr.from, tr.to);
        let mut to_line = (roads.pos[tr.to as usize] - roads.pos[tr.from as usize]).length() - tr.along;
        if phase == 0 {
            // no light on this stretch: look one stretch on, straight ahead (the road is cut into
            // pieces of about 8 m, too short to brake in from speed)
            let d_in = (roads.pos[tr.to as usize] - roads.pos[tr.from as usize]).with_y(0.0).normalize_or_zero();
            let ahead = roads.adj[tr.to as usize]
                .iter()
                .filter(|&&n| n != tr.from)
                .map(|&n| (n, (roads.pos[n as usize] - roads.pos[tr.to as usize]).with_y(0.0).normalize_or_zero().dot(d_in)))
                .filter(|x| x.1 > 0.8)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((n, _)) = ahead {
                phase = roads.link_light(tr.to, n);
                to_line += (roads.pos[n as usize] - roads.pos[tr.to as usize]).length();
            }
        }
        tr.at_red = phase != 0 && tr.panic <= 0.0 && !light_green(phase, time.elapsed_secs()) && !(to_line < 2.5 && tr.speed > 3.0 && !tr.at_red);
        if tr.at_red {
            want = want.min(if to_line < 1.5 { 0.0 } else { cruise * ((to_line - 1.5) / 13.0).min(1.0) });
        }
        if person < 18.0 {
            want = want.min(if person < 5.0 { 0.0 } else { cruise * (person - 5.0) / 13.0 });
        }
        // a turn at the end of this stretch: IV's traffic takes turns at about 4 m/s (middle half
        // 3.4 to 4.6 over 249 captured turns) and does NOT otherwise slow or give way at junctions
        // without lights (it stops there no more often than on plain road: re/giveway2.py).
        // How early he brakes (1.5 m/s2) is ours.
        let next = match tr.next {
            Some(n) => n,
            None => roads.next(tr.from, tr.to),
        };
        tr.next = Some(next);
        tr.turning = false;
        if next != NO_WAY && tr.panic <= 0.0 {
            let d_in = (roads.pos[tr.to as usize] - roads.pos[tr.from as usize]).with_y(0.0).normalize_or_zero();
            let d_out = (roads.pos[next as usize] - roads.pos[tr.to as usize]).with_y(0.0).normalize_or_zero();
            if d_in.dot(d_out) < 0.64 {
                let corner = 3.4 + (tr.cruise - 7.0) / 6.0 * 1.2;
                let to_node = (roads.pos[tr.to as usize] - roads.pos[tr.from as usize]).length() - tr.along;
                tr.turning = true;
                want = want.min((corner * corner + 2.0 * 1.5 * to_node.max(0.0)).sqrt());
            }
        }
        if person < 8.0 && tr.speed < 1.0 {
            let before = tr.held_up;
            tr.held_up += dt;
            // the horn after 1.5 s, then every 3 s or so
            if before < 1.5 && tr.held_up >= 1.5 || (tr.held_up > 1.5 && (before / 3.0).floor() != (tr.held_up / 3.0).floor()) {
                honk.write(crate::sound::Honk { model: car.model.clone(), at: t.translation });
            }
        } else {
            tr.held_up = 0.0;
        }
        tr.panic = (tr.panic - dt).max(0.0);
        if tr.duck > 0.0 {
            tr.duck -= dt;
            want = 0.0;
        }
        let accel = if want > tr.speed { if tr.panic > 0.0 { 5.0 } else { 2.5 } } else { 7.0 };
        tr.speed += (want - tr.speed).clamp(-accel * dt, accel * dt);
        tr.along += tr.speed * dt;
        let len = (roads.pos[tr.to as usize] - roads.pos[tr.from as usize]).length();
        if tr.along >= len {
            tr.along -= len;
            tr.next = None;
            if next == NO_WAY {
                commands.entity(e).despawn();
                continue;
            }
            tr.prev = tr.from;
            tr.prev_lane = tr.lane;
            tr.from = tr.to;
            tr.to = next;
            // keep his lane where the new road has it, else the outermost one it has
            tr.lane = tr.lane.min(roads.link_lanes(tr.from, tr.to).0.saturating_sub(1));
        }
        // through a node he follows the corner curve, position and heading both: the end of this
        // stretch into the next one, or the start of it out of the last one
        let len = (roads.pos[tr.to as usize] - roads.pos[tr.from as usize]).length();
        let bend = tr
            .next
            .filter(|&n| n != NO_WAY)
            .and_then(|n| corner(&roads, tr.from, tr.to, n, tr.lane, tr.along - len))
            .or_else(|| if tr.prev != NO_WAY { corner(&roads, tr.prev, tr.from, tr.to, tr.prev_lane, tr.along) } else { None });
        let (p, dir) = bend.unwrap_or_else(|| lane_point(&roads, tr.from, tr.to, tr.along, tr.lane));
        let target = (-dir.x).atan2(-dir.z);
        if bend.is_some() {
            tr.yaw = target;
        } else {
            // off a curve (straight road, or a U-turn): steer smoothly toward the lane direction
            let mut d = target - tr.yaw;
            while d > std::f32::consts::PI {
                d -= std::f32::consts::TAU;
            }
            while d < -std::f32::consts::PI {
                d += std::f32::consts::TAU;
            }
            tr.yaw += d * (dt * 5.0).min(1.0);
        }
        let g = collision::ground_height(city.collision.values(), p.x, p.z, t.translation.y + 2.0).unwrap_or(p.y);
        // on the curve he's exactly on it; off it, ease only sideways into the lane (a U-turn's
        // jump), never along it, so leaving a curve doesn't make him drop back
        let lane = Vec3::new(p.x, g + tr.ride, p.z);
        let gap = lane - t.translation;
        let ahead = dir * gap.dot(dir);
        let pos = if bend.is_some() || gap.length() >= 6.0 { lane } else { t.translation + ahead + (gap - ahead) * (dt * 6.0).min(1.0) };
        let rot = Quat::from_rotation_y(tr.yaw);
        let vel = rot * Vec3::NEG_Z * tr.speed;
        t.translation = pos;
        t.rotation = rot;
        car.set_pose(pos, rot, vel);
    }
}

/// LIBERTY_ROAD_PROBE: once the city around the camera has loaded, walk sideways from the middle of
/// every nearby link to the kerbs (a ground-height step) and print the road width per link-flags
/// value — ground truth for decoding the lane bits.
pub fn probe_roads(time: Res<Time>, roads: Res<Roads>, city: Res<City>, cam: Query<&Transform, With<Fly>>, mut done: Local<bool>) {
    let _slow = crate::SlowGuard::new("traffic::probe_roads");
    if *done || std::env::var("LIBERTY_ROAD_PROBE").is_err() || time.elapsed_secs() < 12.0 {
        return;
    }
    *done = true;
    let Ok(cam_t) = cam.single() else { return };
    let at = cam_t.translation;
    let mut by: std::collections::BTreeMap<u16, Vec<(f32, f32)>> = Default::default();
    for (a, links) in roads.adj.iter().enumerate() {
        let pa = roads.pos[a];
        if (pa - at).with_y(0.0).length() > 250.0 {
            continue;
        }
        for (k, &b) in links.iter().enumerate() {
            let pb = roads.pos[b as usize];
            let dir = (pb - pa).with_y(0.0).normalize_or_zero();
            if dir == Vec3::ZERO {
                continue;
            }
            let right = Vec3::new(-dir.z, 0.0, dir.x);
            let m = (pa + pb) / 2.0;
            let Some(g0) = collision::ground_height(city.collision.values(), m.x, m.z, m.y + 2.0) else { continue };
            let side = |s: Vec3| {
                let mut d = 0.5;
                while d < 25.0 {
                    let p = m + s * d;
                    match collision::ground_height(city.collision.values(), p.x, p.z, g0 + 1.0) {
                        Some(g) if (g - g0).abs() < 0.08 => d += 0.25,
                        _ => return d,
                    }
                }
                d
            };
            by.entry(roads.link_flags[a][k]).or_default().push((side(-right), side(right)));
        }
    }
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    for (f, w) in v.iter().take(16) {
        let n = w.len() as f32;
        let (l, r) = w.iter().fold((0.0, 0.0), |s, x| (s.0 + x.0, s.1 + x.1));
        let mut tot: Vec<f32> = w.iter().map(|x| x.0 + x.1).collect();
        tot.sort_by(|a, b| a.partial_cmp(b).unwrap());
        info!("probe flags {f:#06x}: {:4} links | left {:5.1} right {:5.1} | median width {:5.1} m", w.len(), l / n, r / n, tot[tot.len() / 2]);
    }
}