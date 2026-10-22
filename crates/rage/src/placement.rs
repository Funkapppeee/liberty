//! Where things go: WPL placement files (binary, inside the map .img archives) and IDE object
//! definitions (plain text next to them). Formats from gtamods.com/wiki/WPL and /wiki/IDE.

use anyhow::{Result, ensure};

/// GTA IV model-name hash: Jenkins one-at-a-time over the lowercase name.
pub fn hash(name: &str) -> u32 {
    let mut h: u32 = 0;
    for b in name.bytes() {
        h = h.wrapping_add(b.to_ascii_lowercase() as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h = h.wrapping_add(h << 15);
    if h < 2 { h + 2 } else { h }
}

#[derive(Debug, Clone, Copy)]
pub struct Instance {
    /// GTA space, Z up.
    pub position: [f32; 3],
    /// Quaternion x, y, z, w.
    pub rotation: [f32; 4],
    pub model_hash: u32,
    pub flags: u32,
    /// Index of this object's LOD parent in the same file, or -1.
    pub lod: i32,
}

/// Object instances from a .wpl (section 0, `inst`).
pub fn read_wpl(bytes: &[u8]) -> Result<Vec<Instance>> {
    ensure!(bytes.len() >= 68, "WPL too short");
    let u = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let f = |at: usize| f32::from_bits(u(at));
    ensure!(u(0) == 3, "WPL version {} (expected 3)", u(0));
    let count = u(4) as usize;
    ensure!(68 + count * 48 <= bytes.len(), "WPL claims {count} instances, file too short");
    Ok((0..count)
        .map(|i| {
            let o = 68 + i * 48;
            Instance {
                position: [f(o), f(o + 4), f(o + 8)],
                rotation: [f(o + 12), f(o + 16), f(o + 20), f(o + 24)],
                model_hash: u(o + 28),
                flags: u(o + 32),
                lod: u(o + 36) as i32,
            }
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct ObjectDef {
    pub name: String,
    pub txd: String,
    pub draw_distance: f32,
}

/// Object definitions from an .ide (`objs`, `tobj`, `anim` sections: name, txd, draw distance, ...).
pub fn read_ide(text: &str) -> Vec<ObjectDef> {
    let mut out = Vec::new();
    let mut in_objects = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line {
            "objs" | "tobj" | "anim" => in_objects = true,
            // any other section header or its end closes the object list
            "end" | "txdp" | "2dfx" | "mlo" | "path" | "tanm" | "amat" | "cars" | "peds" | "weap" => {
                in_objects = false
            }
            _ if in_objects => {
                let mut f = line.split(',').map(str::trim);
                if let (Some(name), Some(txd), Some(dist)) = (f.next(), f.next(), f.next()) {
                    out.push(ObjectDef {
                        name: name.to_ascii_lowercase(),
                        txd: txd.to_ascii_lowercase(),
                        draw_distance: dist.parse().unwrap_or(300.0),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn hash_is_case_insensitive_and_stable() {
        assert_eq!(super::hash("Admiral"), super::hash("admiral"));
        assert_ne!(super::hash("admiral"), super::hash("banshee"));
    }
}
