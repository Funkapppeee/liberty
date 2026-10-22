//! Vehicle definitions from `common/data/handling.dat`, `vehicles.ide` and `carcols.dat` (all
//! plain text). Column layouts from the files' own comment headers; verified against the
//! ADMIRAL line in handling.dat.
//!
//! handling.dat's table header lists a couple of columns (a handbrake force `Thb` between brake
//! bias and steering lock, and a traction spring delta `Ws+` between the lateral traction curve
//! and the traction bias) that aren't in this module's `Handling` struct — they're parsed
//! positionally and dropped, same as the damage/misc columns we don't expose.

use std::collections::HashMap;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Handling {
    pub id: String,
    pub mass: f32,
    pub drag_mult: f32,
    pub centre_of_mass: [f32; 3],
    pub drive_bias_front: f32,
    pub gears: u32,
    pub drive_force: f32,
    pub drive_inertia: f32,
    pub max_velocity_kmh: f32,
    pub brake_force: f32,
    pub brake_bias_front: f32,
    /// Column 14: the handbrake's brake on the rear wheels, in the car's weight.
    pub handbrake: f32,
    pub steering_lock_deg: f32,
    pub traction_curve_max: f32,
    pub traction_curve_min: f32,
    pub traction_curve_lateral_deg: f32,
    pub traction_bias_front: f32,
    pub suspension_force: f32,
    pub suspension_comp_damp: f32,
    pub suspension_rebound_damp: f32,
    pub suspension_upper: f32,
    pub suspension_lower: f32,
    pub suspension_raise: f32,
    pub suspension_bias_front: f32,
    pub model_flags: u32,
    pub handling_flags: u32,
}

/// Handling entries from `handling.dat`, keyed by lowercase id. Lines whose field count doesn't
/// match the 37-column car table (comments, section headers, any non-car rows) are skipped; the
/// caller can compare `text.lines()` against `read_handling(..)?.len()` to see how many.
pub fn read_handling(text: &str) -> Result<HashMap<String, Handling>> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let fields: Vec<&str> = line.split_ascii_whitespace().collect();
        if fields.len() != 37 {
            continue;
        }
        let f = |j: usize| -> Result<f32> {
            fields[j].parse::<f32>().map_err(|e| anyhow::anyhow!("handling.dat {}: {e}", fields[0]))
        };
        let hex = |j: usize| -> Result<u32> {
            u32::from_str_radix(fields[j], 16)
                .map_err(|e| anyhow::anyhow!("handling.dat {}: {e}", fields[0]))
        };
        let Ok(gears) = fields[8].parse::<u32>() else { continue };
        out.insert(
            fields[0].to_ascii_lowercase(),
            Handling {
                id: fields[0].to_string(),
                mass: f(1)?,
                drag_mult: f(2)?,
                centre_of_mass: [f(4)?, f(5)?, f(6)?],
                drive_bias_front: f(7)?,
                gears,
                drive_force: f(9)?,
                drive_inertia: f(10)?,
                max_velocity_kmh: f(11)?,
                brake_force: f(12)?,
                brake_bias_front: f(13)?,
                handbrake: f(14)?,
                steering_lock_deg: f(15)?,
                traction_curve_max: f(16)?,
                traction_curve_min: f(17)?,
                traction_curve_lateral_deg: f(18)?,
                traction_bias_front: f(20)?,
                suspension_force: f(21)?,
                suspension_comp_damp: f(22)?,
                suspension_rebound_damp: f(23)?,
                suspension_upper: f(24)?,
                suspension_lower: f(25)?,
                suspension_raise: f(26)?,
                suspension_bias_front: f(27)?,
                model_flags: hex(34)?,
                handling_flags: hex(35)?,
            },
        );
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct VehicleDef {
    pub model: String,
    pub txd: String,
    pub kind: String,
    pub handling_id: String,
    pub game_name: String,
    pub wheel_scale_front: f32,
    pub wheel_scale_rear: f32,
}

/// Vehicle definitions from the `cars` section of `vehicles.ide` (cars, bikes, boats, helis and
/// trains share the same leading columns: model, txd, type, handlingId, gameName, anims, anims2,
/// frequency, maxNum, wheelScaleFront, wheelScaleRear, ...).
pub fn read_vehicles_ide(text: &str) -> Result<Vec<VehicleDef>> {
    let mut out = Vec::new();
    let mut in_cars = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line {
            "cars" => in_cars = true,
            "end" => in_cars = false,
            _ if in_cars => {
                let fields: Vec<&str> = line.split(',').map(str::trim).collect();
                if fields.len() < 11 {
                    continue;
                }
                out.push(VehicleDef {
                    model: fields[0].to_ascii_lowercase(),
                    txd: fields[1].to_ascii_lowercase(),
                    kind: fields[2].to_ascii_lowercase(),
                    handling_id: fields[3].to_ascii_lowercase(),
                    game_name: fields[4].to_string(),
                    wheel_scale_front: fields[9].parse().unwrap_or(0.0),
                    wheel_scale_rear: fields[10].parse().unwrap_or(0.0),
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct CarColours {
    pub palette: Vec<[u8; 3]>,
    pub combos: HashMap<String, Vec<Vec<u8>>>,
}

/// Colour table and per-model colour combinations from `carcols.dat`. The `col` section is the
/// RGB palette; `car`/`car3`/`car4`/... sections each list, per model, groups of palette indices
/// (group size = the trailing digit in the section name, or 3 for plain `car`).
pub fn read_carcols(text: &str) -> Result<CarColours> {
    let mut out = CarColours::default();
    let mut section: Option<String> = None;
    let mut chunk = 3usize;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "end" {
            section = None;
            continue;
        }
        if line == "col" || (line.starts_with("car") && line[3..].chars().all(|c| c.is_ascii_digit())) {
            chunk = line[3..].parse().unwrap_or(3);
            section = Some(line.to_string());
            continue;
        }
        match section.as_deref() {
            Some("col") => {
                let parts: Vec<&str> = line.split(',').map(str::trim).collect();
                if parts.len() >= 3 {
                    if let (Ok(r), Ok(g), Ok(b)) =
                        (parts[0].parse::<u8>(), parts[1].parse::<u8>(), parts[2].parse::<u8>())
                    {
                        out.palette.push([r, g, b]);
                    }
                }
            }
            Some(s) if s.starts_with("car") => {
                let mut parts = line.splitn(2, ',');
                let (Some(model), Some(rest)) = (parts.next(), parts.next()) else { continue };
                let nums: Vec<u8> = rest
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse::<u8>().ok())
                    .collect();
                let combos: Vec<Vec<u8>> = nums.chunks(chunk).map(<[u8]>::to_vec).collect();
                out.combos.entry(model.trim().to_ascii_lowercase()).or_default().extend(combos);
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_admiral_handling() {
        let text = "ADMIRAL\t     1700.0  6.0   85\t0.0 0.0 -0.2   \t\t0.0  5  0.17  1.0 140.0\t0.22 0.65 0.7  35.0\t\t1.2  0.95 14.0 0.13 0.47\t\t1.6  1.0  1.0  0.15 -0.16 0.0  0.5  \t\t1.0 1.0 0.7 1.5 \t0.0  25000 \t\t440080\t\t0\t\t\t\t0\n";
        let map = read_handling(text).unwrap();
        let h = &map["admiral"];
        assert_eq!(h.id, "ADMIRAL");
        assert_eq!(h.mass, 1700.0);
        assert_eq!(h.drag_mult, 6.0);
        assert_eq!(h.centre_of_mass, [0.0, 0.0, -0.2]);
        assert_eq!(h.drive_force, 0.17);
        assert_eq!(h.gears, 5);
        assert_eq!(h.max_velocity_kmh, 140.0);
        assert_eq!(h.brake_force, 0.22);
        assert_eq!(h.brake_bias_front, 0.65);
        assert_eq!(h.steering_lock_deg, 35.0);
        assert_eq!(h.traction_curve_max, 1.2);
        assert_eq!(h.traction_curve_min, 0.95);
        assert_eq!(h.traction_curve_lateral_deg, 14.0);
        assert_eq!(h.traction_bias_front, 0.47);
        assert_eq!(h.suspension_force, 1.6);
        assert_eq!(h.suspension_bias_front, 0.5);
        assert_eq!(h.model_flags, 0x440080);
        assert_eq!(h.handling_flags, 0);
    }

    #[test]
    fn skips_lines_with_wrong_field_count() {
        let text = "; comment\nADMIRAL 1 2 3\n";
        let map = read_handling(text).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn parses_vehicles_ide() {
        let text = "cars\nadmiral, \tadmiral,\tcar,\t\tADMIRAL,\tADMIRAL,\tVEH@STD,\tVEH@STD_LO4_LO4,\t100,\t999,\t\t0.2229,\t0.2229,\t\t0,\t2,\t1.0\t,0,\t-\nend\n";
        let defs = read_vehicles_ide(text).unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].model, "admiral");
        assert_eq!(defs[0].kind, "car");
        assert_eq!(defs[0].handling_id, "admiral");
        assert_eq!(defs[0].game_name, "ADMIRAL");
        assert_eq!(defs[0].wheel_scale_front, 0.2229);
        assert_eq!(defs[0].wheel_scale_rear, 0.2229);
    }

    #[test]
    fn parses_carcols() {
        let text = "col\n10,10,10,-,black\t\t\t# 0 black\n37,37,39,-,black\t\t\t# 1 black poly\nend\n\ncar3\nadmiral, \t0,0,22, 7,7,79,\nend\n";
        let cols = read_carcols(text).unwrap();
        assert_eq!(cols.palette, vec![[10, 10, 10], [37, 37, 39]]);
        assert_eq!(cols.combos["admiral"], vec![vec![0, 0, 22], vec![7, 7, 79]]);
    }
}
