//! Water quads from `common/data/water.dat` (plain text, whitespace-separated). Each line is 4
//! vertices (x, y, z, then 4 unknown per-vertex params — likely flow/wave/opacity), an integer
//! kind and a trailing float flag. From gtamods.com and inspection of the shipped file.

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy)]
pub struct WaterQuad {
    /// v0=(x1,y1), v1=(x2,y1), v2=(x1,y2), v3=(x2,y2); triangles (0,1,2) and (2,1,3).
    pub corners: [[f32; 3]; 4],
    pub params: [[f32; 4]; 4],
    pub kind: u32,
    pub flag: f32,
}

/// Water quads from `water.dat`. Degenerate quads (all 4 corners identical) are skipped.
pub fn read_water(text: &str) -> Result<Vec<WaterQuad>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let n = i + 1;
        let fields: Vec<&str> = line.split_ascii_whitespace().collect();
        if fields.len() != 30 {
            bail!("water.dat line {n}: expected 30 fields, found {}", fields.len());
        }
        let f = |j: usize| -> Result<f32> {
            fields[j].parse::<f32>().map_err(|e| anyhow::anyhow!("water.dat line {n}: {e}"))
        };
        let mut corners = [[0f32; 3]; 4];
        let mut params = [[0f32; 4]; 4];
        for v in 0..4 {
            let o = v * 7;
            corners[v] = [f(o)?, f(o + 1)?, f(o + 2)?];
            params[v] = [f(o + 3)?, f(o + 4)?, f(o + 5)?, f(o + 6)?];
        }
        let kind: u32 = fields[28]
            .parse()
            .map_err(|e| anyhow::anyhow!("water.dat line {n}: bad kind: {e}"))?;
        let flag = f(29)?;
        if corners[0] == corners[1] && corners[0] == corners[2] && corners[0] == corners[3] {
            continue;
        }
        out.push(WaterQuad { corners, params, kind, flag });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_examples_and_skips_degenerate() {
        let text = "\
-966.0 664.0 0.0001 0.0 0.0 1.0 0.993   -976.0 664.0 0.0001 0.0 0.0 1.0 0.987   -966.0 674.0 0.0001 0.0 0.0 1.0 1.0   -976.0 674.0 0.0001 0.0 0.0 1.0 0.995 5 1.0
32.0 126.0 14.4375 0.0 0.0 0.0 0.0   48.0 126.0 14.4375 0.0 0.0 0.0 0.0   32.0 110.0 14.4375 0.0 0.0 0.0 0.0   48.0 110.0 14.4375 0.0 0.0 0.0 0.0 13 0.0
-504.0 -8.0 0.0 0.0 0.0 0.0 0.0   -504.0 -8.0 0.0 0.0 0.0 0.0 0.0   -504.0 -8.0 0.0 0.0 0.0 0.0 0.0   -504.0 -8.0 0.0 0.0 0.0 0.0 0.0 5 0.0
";
        let quads = read_water(text).unwrap();
        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0].kind, 5);
        assert_eq!(
            quads[0].corners,
            [
                [-966.0, 664.0, 0.0001],
                [-976.0, 664.0, 0.0001],
                [-966.0, 674.0, 0.0001],
                [-976.0, 674.0, 0.0001],
            ]
        );
    }
}
