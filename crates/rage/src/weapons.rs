//! `common/data/WeaponInfo.xml`: every weapon's damage, range, rate of fire, clip, reload time,
//! model and animation group. Plain XML in the user's install; no decryption.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Weapon {
    /// the game's name, e.g. `PISTOL`
    pub name: String,
    pub slot: String,
    /// `MELEE`, `INSTANT_HIT`, `PROJECTILE`, ...
    pub fire_type: String,
    pub damage_type: String,
    pub group: String,
    pub target_range: f32,
    pub weapon_range: f32,
    pub clip_size: u32,
    pub ammo_max: u32,
    /// milliseconds between shots
    pub time_between_shots_ms: f32,
    pub damage: f32,
    /// impulse applied to what it hits
    pub force: f32,
    pub reload_ms: f32,
    pub accuracy: f32,
    /// ammo lying around when it's dropped
    pub ammo_on_street: u32,
    /// aiming offset from the ped (x right, y forward, z up) standing and crouched
    pub aim_offset: [f32; 3],
    pub aim_offset_crouched: [f32; 3],
    pub flags: Vec<String>,
    /// model name in `weapons.img`, e.g. `w_glock`
    pub model: String,
    /// animation pack for the ped holding it, e.g. `gun@handgun`
    pub anim_group: String,
    pub muzzle_fx: String,
    pub shell_fx: String,
}

impl Weapon {
    pub fn has_flag(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
    pub fn is_gun(&self) -> bool {
        self.has_flag("GUN")
    }
    /// shots per second the weapon fires at while the trigger is held
    pub fn rate_of_fire(&self) -> f32 {
        if self.time_between_shots_ms > 0.0 { 1000.0 / self.time_between_shots_ms } else { 0.0 }
    }
}

struct Tag<'a> {
    name: &'a str,
    attrs: HashMap<&'a str, &'a str>,
    closing: bool,
    /// text up to the next tag (for `<flag>NAME</flag>`)
    text: &'a str,
}

/// every `<tag ...>` in the file, in order, skipping comments and the prolog
fn tags(src: &str) -> Vec<Tag<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = src[i..].find('<') {
        let start = i + off;
        if src[start..].starts_with("<!--") {
            i = src[start..].find("-->").map_or(src.len(), |e| start + e + 3);
            continue;
        }
        let Some(end_off) = src[start..].find('>') else { break };
        let end = start + end_off;
        let mut inner = &src[start + 1..end];
        let closing = inner.starts_with('/');
        if closing {
            inner = &inner[1..];
        }
        inner = inner.trim_end_matches('/');
        let (name, rest) = inner.split_once(|c: char| c.is_whitespace()).unwrap_or((inner, ""));
        let mut attrs = HashMap::new();
        let mut r = rest;
        while let Some(eq) = r.find('=') {
            let key = r[..eq].trim();
            let after = &r[eq + 1..];
            let Some(q0) = after.find('"') else { break };
            let Some(q1) = after[q0 + 1..].find('"') else { break };
            attrs.insert(key, &after[q0 + 1..q0 + 1 + q1]);
            r = &after[q0 + q1 + 2..];
        }
        let next = src[end + 1..].find('<').map_or(src.len(), |n| end + 1 + n);
        out.push(Tag { name, attrs, closing, text: src[end + 1..next].trim() });
        i = end + 1;
    }
    out
}

fn f(t: &Tag, k: &str) -> Option<f32> {
    t.attrs.get(k)?.parse().ok()
}

/// parse the weapon list from the XML text
pub fn parse(src: &str) -> Vec<Weapon> {
    let mut weapons: Vec<Weapon> = Vec::new();
    let mut cur: Option<Weapon> = None;
    for t in tags(src) {
        match (t.name, t.closing) {
            ("weapon", false) => {
                cur = Some(Weapon { name: t.attrs.get("type").unwrap_or(&"").to_string(), ..Default::default() });
            }
            ("weapon", true) => weapons.extend(cur.take()),
            (name, false) => {
                let Some(w) = cur.as_mut() else { continue };
                let s = |k: &str| t.attrs.get(k).map(|v| v.to_string()).unwrap_or_default();
                match name {
                    "data" => {
                        w.slot = s("slot");
                        w.fire_type = s("firetype");
                        w.damage_type = s("damagetype");
                        w.group = s("group");
                        w.target_range = f(&t, "targetrange").unwrap_or(0.0);
                        w.weapon_range = f(&t, "weaponrange").unwrap_or(0.0);
                        w.clip_size = f(&t, "clipsize").unwrap_or(0.0) as u32;
                        w.ammo_max = f(&t, "ammomax").unwrap_or(0.0) as u32;
                        w.time_between_shots_ms = f(&t, "timebetweenshots").unwrap_or(0.0);
                    }
                    "damage" => w.damage = f(&t, "base").unwrap_or(0.0),
                    "physics" => w.force = f(&t, "force").unwrap_or(0.0),
                    "reload" => w.reload_ms = f(&t, "time").unwrap_or(0.0),
                    "aiming" => w.accuracy = f(&t, "accuracy").unwrap_or(0.0),
                    "offset" => w.aim_offset = [f(&t, "x").unwrap_or(0.0), f(&t, "y").unwrap_or(0.0), f(&t, "z").unwrap_or(0.0)],
                    "crouchedoffset" => w.aim_offset_crouched = [f(&t, "x").unwrap_or(0.0), f(&t, "y").unwrap_or(0.0), f(&t, "z").unwrap_or(0.0)],
                    "pickup" => w.ammo_on_street = f(&t, "ammoonstreet").unwrap_or(0.0) as u32,
                    "flag" => w.flags.push(t.text.to_string()),
                    "assets" => w.model = s("model"),
                    "anim" if t.attrs.contains_key("group") => w.anim_group = s("group"),
                    "muzzle" => w.muzzle_fx = s("fx"),
                    "shell" => w.shell_fx = s("fx"),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    weapons
}

/// read `<install>/common/data/WeaponInfo.xml`
pub fn load(root: &Path) -> Result<Vec<Weapon>> {
    let p = root.join("common/data/WeaponInfo.xml");
    let src = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    Ok(parse(&src))
}
