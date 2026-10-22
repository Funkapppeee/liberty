//! RSC5 resources: a 12-byte header (`RSC\x05`, type, flags) then a zlib stream that inflates to
//! the "system" segment (CPU structs) followed by the "graphics" segment (vertex/index/pixel data).
//! Pointers inside are 0x5xxxxxxx for system offsets and 0x6xxxxxxx for graphics offsets.

use std::io::Read;

use anyhow::{Context, Result, bail, ensure};
use flate2::read::{DeflateDecoder, ZlibDecoder};

pub const MAGIC: u32 = 0x0543_5352; // "RSC\x05"

pub struct Resource {
    pub kind: u32,
    pub system: Vec<u8>,
    pub graphics: Vec<u8>,
}

/// Segment sizes packed into the flags: an 11-bit count shifted by a 4-bit exponent (+8).
pub fn segment_sizes(flags: u32) -> (usize, usize) {
    let system = ((flags & 0x7FF) as usize) << (((flags >> 11) & 0xF) + 8);
    let graphics = (((flags >> 15) & 0x7FF) as usize) << (((flags >> 26) & 0xF) + 8);
    (system, graphics)
}

impl Resource {
    pub fn load(bytes: &[u8]) -> Result<Resource> {
        ensure!(bytes.len() > 12, "resource too short");
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        if word(0) != MAGIC {
            bail!("not an RSC5 resource (magic {:08X})", word(0));
        }
        let (kind, flags) = (word(4), word(8));
        let (sys_len, gfx_len) = segment_sizes(flags);

        let body = &bytes[12..];
        let mut out = Vec::with_capacity(sys_len + gfx_len);
        if body.first() == Some(&0x78) {
            ZlibDecoder::new(body).read_to_end(&mut out)
        } else {
            DeflateDecoder::new(body).read_to_end(&mut out)
        }
        .context("inflating resource")?;
        ensure!(
            out.len() >= sys_len + gfx_len,
            "resource inflated to {} bytes, header promised {} + {}",
            out.len(),
            sys_len,
            gfx_len
        );
        let graphics = out.split_off(sys_len);
        Ok(Resource { kind, system: out, graphics })
    }

    /// Resolve a pointer to (segment, offset).
    pub fn at(&self, ptr: u32) -> Result<(&[u8], usize)> {
        let off = (ptr & 0x0FFF_FFFF) as usize;
        let seg = match ptr >> 28 {
            5 => &self.system,
            6 => &self.graphics,
            _ => bail!("not a resource pointer: {ptr:08X}"),
        };
        ensure!(off < seg.len(), "pointer {ptr:08X} past segment end ({})", seg.len());
        Ok((seg, off))
    }

    pub fn u32(&self, ptr: u32, field: usize) -> Result<u32> {
        let (seg, off) = self.at(ptr)?;
        let b = seg.get(off + field..off + field + 4).context("field past segment end")?;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn u16(&self, ptr: u32, field: usize) -> Result<u16> {
        let (seg, off) = self.at(ptr)?;
        let b = seg.get(off + field..off + field + 2).context("field past segment end")?;
        Ok(u16::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn u8(&self, ptr: u32, field: usize) -> Result<u8> {
        let (seg, off) = self.at(ptr)?;
        seg.get(off + field).copied().context("field past segment end")
    }

    pub fn cstr(&self, ptr: u32) -> Result<String> {
        let (seg, off) = self.at(ptr)?;
        let end = seg[off..].iter().position(|&b| b == 0).context("unterminated string")?;
        Ok(String::from_utf8_lossy(&seg[off..off + end]).into_owned())
    }

    pub fn bytes(&self, ptr: u32, len: usize) -> Result<&[u8]> {
        let (seg, off) = self.at(ptr)?;
        seg.get(off..off + len).context("data past segment end")
    }

    /// The root object always sits at the start of the system segment.
    pub const ROOT: u32 = 0x5000_0000;
}
