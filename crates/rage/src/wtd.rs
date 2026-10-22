//! WTD texture dictionaries: `pgDictionary<grcTexturePC>`.
//! Struct offsets from GTA-Network/IV-Network `IV-RSCEN.txt`, checked against real files.

use anyhow::{Result, bail};

use crate::rsc::Resource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Dxt1,
    Dxt3,
    Dxt5,
    /// D3DFMT_A8R8G8B8 — BGRA bytes in memory.
    Argb8,
    /// D3DFMT_L8 — one grey byte per pixel.
    L8,
}

impl Format {
    fn from_d3d(v: u32) -> Option<Format> {
        Some(match &v.to_le_bytes() {
            b"DXT1" => Format::Dxt1,
            b"DXT3" => Format::Dxt3,
            b"DXT5" => Format::Dxt5,
            _ => match v {
                21 => Format::Argb8,
                50 => Format::L8,
                _ => return None,
            },
        })
    }

    /// Bytes for the top mip level.
    pub fn level0_len(self, w: usize, h: usize) -> usize {
        let blocks = w.div_ceil(4) * h.div_ceil(4);
        match self {
            Format::Dxt1 => blocks * 8,
            Format::Dxt3 | Format::Dxt5 => blocks * 16,
            Format::Argb8 => w * h * 4,
            Format::L8 => w * h,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub format: Format,
    /// Mip levels actually present in `data` (the header count, trimmed if the file is short).
    pub levels: u8,
    /// Every mip level back to back, still compressed (as the GPU wants it).
    pub data: Vec<u8>,
}

impl Texture {
    pub fn level0(&self) -> &[u8] {
        &self.data[..self.format.level0_len(self.width as usize, self.height as usize)]
    }
}

/// Byte length of mip levels 0..levels.
pub fn mips_len(format: Format, w: usize, h: usize, levels: u8) -> usize {
    (0..levels as u32).map(|i| format.level0_len((w >> i).max(1), (h >> i).max(1))).sum()
}

/// A .wtd file: the dictionary sits at the resource root.
pub fn read(res: &Resource) -> Result<Vec<Texture>> {
    read_at(res, Resource::ROOT)
}

/// A texture dictionary at any pointer (e.g. embedded in a drawable's shader group).
pub fn read_at(res: &Resource, root: u32) -> Result<Vec<Texture>> {
    let hashes = res.u32(root, 0x10)?;
    let list = res.u32(root, 0x18)?;
    let count = res.u16(root, 0x1C)? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let t = res.u32(list, i * 4)?;
        // Some LOD dictionaries (slod_*) carry no name strings, only the name hashes.
        let name = match res.u32(t, 0x14)? {
            0 => format!("hash_{:08x}", res.u32(hashes, i * 4)?),
            p => crate::drawable::normalise_name(&res.cstr(p)?),
        };
        // Pixel pointer is at +0x48 in type-8 dictionaries but +0x44 in the older type-7 ones
        // (slod_*); take whichever field points into the graphics segment.
        let is_gfx = |p: u32| p >> 28 == 6;
        let pixels = match (res.u32(t, 0x48)?, res.u32(t, 0x44)?) {
            (p, _) if is_gfx(p) => p,
            (_, p) if is_gfx(p) => p,
            _ => continue, // placeholder entry with no pixel data
        };
        let width = res.u16(t, 0x1C)?;
        let height = res.u16(t, 0x1E)?;
        let fmt_raw = res.u32(t, 0x20)?;
        let Some(format) = Format::from_d3d(fmt_raw) else {
            bail!("texture {name}: unknown D3D format {fmt_raw:#X}");
        };
        let (w, h) = (width as usize, height as usize);
        let (seg, off) = res.at(pixels)?;
        let available = seg.len() - off;
        let mut levels = res.u8(t, 0x27)?.max(1);
        while levels > 1 && mips_len(format, w, h, levels) > available {
            levels -= 1;
        }
        let data = res.bytes(pixels, mips_len(format, w, h, levels))?.to_vec();
        out.push(Texture { name, width, height, format, levels, data });
    }
    Ok(out)
}
