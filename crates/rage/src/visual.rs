//! `common/data/visualSettings.dat`: the game's table of named numbers for how things look
//! (car headlights and tail lights, traffic lights, rain, sky, camera blur, LOD distances).
//! Lines are `name<tab>value`; `#` starts a comment.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

pub struct Visual(pub HashMap<String, f32>);

impl Visual {
    pub fn load(root: &Path) -> Result<Visual> {
        let text = std::fs::read_to_string(root.join("common/data/visualSettings.dat")).context("visualSettings.dat")?;
        Ok(Visual(parse(&text)))
    }

    pub fn get(&self, name: &str, default: f32) -> f32 {
        self.0.get(name).copied().unwrap_or(default)
    }

    /// `<name>.red`, `.green`, `.blue`.
    pub fn rgb(&self, name: &str, default: [f32; 3]) -> [f32; 3] {
        [self.get(&format!("{name}.red"), default[0]), self.get(&format!("{name}.green"), default[1]), self.get(&format!("{name}.blue"), default[2])]
    }
}

pub fn parse(text: &str) -> HashMap<String, f32> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(value)) = (parts.next(), parts.next()) {
            if !name.starts_with('#') {
                if let Ok(v) = value.parse() {
                    out.insert(name.to_string(), v);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_names_and_skips_comments() {
        let v = super::parse("# car.headlight.color\t1.00\t1.00\t0.80\ncar.headlight.intensity\t3.00\t\t\n\ncar.headlight.angle\t-0.20\n");
        assert_eq!(v.len(), 2);
        assert_eq!(v["car.headlight.intensity"], 3.0);
        assert_eq!(v["car.headlight.angle"], -0.2);
    }
}
