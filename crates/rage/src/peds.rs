//! Pedestrian definitions from `common/data/pedVariations.dat`, `peds.ide` and `moveblend.dat`
//! (all plain text). Column layouts from the files' own comment headers; verified against the
//! M_Y_PCool_01 block in pedVariations.dat.

use std::collections::HashMap;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct SlotVariant {
    pub slot: String,
    pub geometry: u32,
    /// BULK, JOB, SUNNY, WET, MAT1, RES1A, RES1B, RES2A, RES2B.
    pub flags: [i32; 9],
    pub audio_id: i32,
}

impl SlotVariant {
    /// Slots starting `p_` are props (pedprops.img), not skinned component drawables.
    pub fn is_prop(&self) -> bool {
        self.slot.starts_with("p_")
    }
}

#[derive(Debug, Clone)]
pub struct PedVariation {
    pub model: String,
    pub slots: Vec<SlotVariant>,
}

/// Ped component/prop variations from `pedVariations.dat`: one block per model — a header line
/// `model,	slot-row count`, a `#`-prefixed column comment, then rows
/// `slot, geometry, BULK, JOB, SUNNY, WET, MAT1, RES1A, RES1B, RES2A, RES2B, AUDIO ID`, ended by
/// `end`. Keyed by lowercase model name.
pub fn read_ped_variations(text: &str) -> Result<HashMap<String, PedVariation>> {
    let mut out = HashMap::new();
    let mut current: Option<PedVariation> = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "end" {
            if let Some(v) = current.take() {
                out.insert(v.model.to_ascii_lowercase(), v);
            }
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if current.is_none() {
            // header line: model, slot-row count
            current = Some(PedVariation { model: fields[0].to_string(), slots: Vec::new() });
            continue;
        }
        if fields.len() < 12 {
            continue;
        }
        let n = |j: usize| fields[j].parse::<i32>().unwrap_or(0);
        current.as_mut().unwrap().slots.push(SlotVariant {
            slot: fields[0].to_ascii_lowercase(),
            geometry: n(1).max(0) as u32,
            flags: [n(2), n(3), n(4), n(5), n(6), n(7), n(8), n(9), n(10)],
            audio_id: n(11),
        });
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct PedDef {
    pub model: String,
    pub props: String,
    pub ped_type: String,
    /// Lowercase movement group (moveblend.dat / anim.img `.wad` name).
    pub move_group: String,
    pub gestures: String,
    pub facials: String,
    /// First voice column (last-but-one field), when present.
    pub voice: String,
}

/// Ped definitions from the `peds` section of `peds.ide`: model, props, pedtype, animgroup,
/// gesturegroup, gesturephonegroup, facialgroup, visemegroup, flags, animfile, blendshapefile,
/// radio1, radio2, audiotype, firstvoice, lastvoice. A handful of rows are missing a comma
/// somewhere (typos in the game's own file); since every field is a single token, we recover
/// the intended columns by also splitting on whitespace.
pub fn read_peds_ide(text: &str) -> Result<Vec<PedDef>> {
    let mut out = Vec::new();
    let mut in_peds = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line {
            "peds" => in_peds = true,
            "end" => in_peds = false,
            _ if in_peds => {
                let fields: Vec<&str> =
                    line.split(',').flat_map(str::split_whitespace).collect();
                if fields.len() < 7 {
                    continue;
                }
                let voice = if fields.len() >= 2 { fields[fields.len() - 2] } else { "" };
                out.push(PedDef {
                    model: fields[0].to_ascii_lowercase(),
                    props: fields[1].to_ascii_lowercase(),
                    ped_type: fields[2].to_string(),
                    move_group: fields[3].to_ascii_lowercase(),
                    gestures: fields[4].to_string(),
                    facials: fields[6].to_string(),
                    voice: voice.to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Movement group fallbacks from `moveblend.dat`: lines `MOVEMENT_GROUP <name> FALLBACK_GROUP
/// <fallback>`. Keyed and valued lowercase.
pub fn read_moveblend(text: &str) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for line in text.lines().map(str::trim) {
        let fields: Vec<&str> = line.split_ascii_whitespace().collect();
        if fields.len() == 4 && fields[0] == "MOVEMENT_GROUP" && fields[2] == "FALLBACK_GROUP" {
            out.insert(fields[1].to_ascii_lowercase(), fields[3].to_ascii_lowercase());
        }
    }
    Ok(out)
}

/// The three possible drawable-dictionary names for a component slot/geometry pair — which one
/// exists varies per ped (e.g. `head_000_r`, `uppr_000_u`).
pub fn drawable_names(slot: &str, geometry: u32) -> [String; 3] {
    ["u", "r", "m"].map(|suffix| format!("{slot}_{geometry:03}_{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ped_variations_block() {
        let text = "M_Y_PCool_01,\t\t9\n#slot\t\tgeometry\tBULK\tJOB\tSUNNY\tWET\tMAT1\tRES1A\tRES1B\tRES2A\tRES2B\t\tAUDIO ID\nfeet,\t\t0,\t\t0,\t0,\t0,\t0,\t0,\t-1,\t-1,\t-1,\t-1,\t\t2\nhead,\t\t0,\t\t0,\t0,\t0,\t0,\t0,\t100,\t1,\t-1,\t-1,\t\t10\np_head,\t\t0,\t\t0,\t0,\t0,\t0,\t0,\t-1,\t-1,\t-1,\t-1,\t\t0\nend\n";
        let map = read_ped_variations(text).unwrap();
        let v = &map["m_y_pcool_01"];
        assert_eq!(v.model, "M_Y_PCool_01");
        assert_eq!(v.slots.len(), 3);
        assert_eq!(v.slots[0].slot, "feet");
        assert_eq!(v.slots[0].geometry, 0);
        assert_eq!(v.slots[0].flags, [0, 0, 0, 0, 0, -1, -1, -1, -1]);
        assert_eq!(v.slots[0].audio_id, 2);
        assert!(!v.slots[0].is_prop());
        assert_eq!(v.slots[1].flags[5], 100);
        assert!(v.slots[2].is_prop());
    }

    #[test]
    fn parses_peds_ide() {
        let text = "peds\nplayer, \t\tplayer_p,\t\tPLAYER1, \t\tmove_player,\t\tGESTURES@MALE,\t\tGESTURES@M_PHONE,\tFACIALS@M_HI,\t\tvisemes@m_hi,\t\t0, \t\tmove_player, \t\tnull,\t\t\t-1,-1,\t\tPED_TYPE_PLAYER,\tVOICE_PLY_CR,  \tVOICE_PLY_CR    \nend\n";
        let defs = read_peds_ide(text).unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].model, "player");
        assert_eq!(defs[0].props, "player_p");
        assert_eq!(defs[0].ped_type, "PLAYER1");
        assert_eq!(defs[0].move_group, "move_player");
        assert_eq!(defs[0].gestures, "GESTURES@MALE");
        assert_eq!(defs[0].facials, "FACIALS@M_HI");
        assert_eq!(defs[0].voice, "VOICE_PLY_CR");
    }

    #[test]
    fn parses_moveblend() {
        let text = "MOVEMENT_GROUP\tmove_f@armed\t\t\tFALLBACK_GROUP\tmove_f@generic\n";
        let map = read_moveblend(text).unwrap();
        assert_eq!(map["move_f@armed"], "move_f@generic");
    }

    #[test]
    fn builds_drawable_names() {
        assert_eq!(
            drawable_names("head", 0),
            ["head_000_u".to_string(), "head_000_r".to_string(), "head_000_m".to_string()]
        );
    }
}
