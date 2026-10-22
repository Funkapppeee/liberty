//! RPF2 archives (GTA IV). Layout verified on `pc/models/cdimages/playerped.rpf`:
//! - 20-byte header: u32 magic ("RPF2" as `0x32465052`), u32 TOC size, u32 entry count,
//!   u32 unknown, u32 encrypted flag (0 = plain, else e.g. `0xFFFFFFFF`).
//! - TOC lives at file offset 0x800, `toc_size` bytes; decrypted as one block if encrypted.
//! - `entry_count` 16-byte entries (4 LE u32s), then a NUL-terminated name table; `w0` is the
//!   name's offset into that table.
//! - Directory iff `w2 & 0x8000_0000`: first child index `w2 & 0x7FFF_FFFF`, child count `w3`.
//!   Entry 0 is the root "/".
//! - File otherwise: `w1` size, file offset `w2 & 0xFFFF_FF00`, resource type `w2 & 0xFF`,
//!   `w3` RSC flags. Resource files keep their normal 12-byte `RSC\x05` header, so `read()` just
//!   returns the stored bytes.
//!
//! RPF3 (the audio archives in `pc/audio/sfx`, magic `0x33465052`) is the same layout with one
//! change: there is no name table, `w0` is the name's hash (`placement::hash`). Entries come out
//! named `0x<hash>` until `resolve_names` is given names that hash to them (`audio::names` collects
//! them from the game's own audio config).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::crypto::Key;

const MAGIC: u32 = 0x3246_5052;
const MAGIC3: u32 = 0x3346_5052;
const TOC_OFFSET: u64 = 0x800;

#[derive(Debug, Clone)]
pub struct Entry {
    /// Full path from the archive root (e.g. `dir/name.wdr`), lowercase.
    pub name: String,
    pub size: u64,
    pub offset: u64,
    /// Resource type byte (0x6E model, 0x08 textures, ...).
    pub kind: u8,
    /// RSC flags copied from the resource header.
    pub flags: u32,
}

pub struct Rpf {
    pub path: PathBuf,
    /// 2 or 3.
    pub version: u8,
    pub encrypted: bool,
    pub entries: Vec<Entry>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// One raw 16-byte TOC record, before it's resolved into a name / directory tree.
struct Raw {
    name_off: u32,
    w1: u32,
    w2: u32,
    w3: u32,
}

impl Rpf {
    pub fn open(path: &Path, key: &Key) -> Result<Rpf> {
        let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut head = [0u8; 20];
        f.read_exact(&mut head)?;
        let magic = u32_at(&head, 0);
        if magic != MAGIC && magic != MAGIC3 {
            bail!("{}: not an RPF2/RPF3 archive (magic {magic:08X})", path.display());
        }
        let version = if magic == MAGIC3 { 3 } else { 2 };
        let toc_size = u32_at(&head, 4) as usize;
        let count = u32_at(&head, 8) as usize;
        let encrypted = u32_at(&head, 16) != 0;
        ensure!(count * 16 <= toc_size, "RPF TOC too small for {count} entries");

        f.seek(SeekFrom::Start(TOC_OFFSET))?;
        let mut toc = vec![0u8; toc_size];
        f.read_exact(&mut toc)?;
        if encrypted {
            key.decrypt(&mut toc);
        }

        let raw: Vec<Raw> = (0..count)
            .map(|i| {
                let e = &toc[i * 16..];
                Raw { name_off: u32_at(e, 0), w1: u32_at(e, 4), w2: u32_at(e, 8), w3: u32_at(e, 12) }
            })
            .collect();
        let names = &toc[count * 16..];
        let name_at = |off: u32| -> String {
            if version == 3 {
                return format!("0x{off:08x}");
            }
            let off = off as usize;
            let end = names[off..].iter().position(|&b| b == 0).unwrap_or(names.len() - off);
            String::from_utf8_lossy(&names[off..off + end]).into_owned()
        };

        let file_len = f.metadata()?.len();
        let mut entries = Vec::new();
        walk(&raw, &name_at, 0, "", &mut entries)?;
        if let Some(bad) = entries.iter().find(|e| e.offset + e.size > file_len) {
            bail!("{}: entry {} points past the end of the file", path.display(), bad.name);
        }
        Ok(Rpf { path: path.to_owned(), version, encrypted, entries })
    }

    /// RPF3: give the `0x<hash>` path parts their names, from a hash -> name table.
    pub fn resolve_names(&mut self, names: &std::collections::HashMap<u32, String>) {
        for e in &mut self.entries {
            let parts: Vec<String> = e
                .name
                .split('/')
                .map(|p| {
                    p.strip_prefix("0x")
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .and_then(|h| names.get(&h))
                        .map_or_else(|| p.to_string(), |n| n.to_ascii_lowercase())
                })
                .collect();
            e.name = parts.join("/");
        }
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| {
            e.name.eq_ignore_ascii_case(name)
                || e.name.rsplit_once('/').is_some_and(|(_, n)| n.eq_ignore_ascii_case(name))
        })
    }

    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(entry.offset))?;
        let mut buf = vec![0u8; entry.size as usize];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// Recursively resolve a directory entry (`idx`) into `out`, prefixing child names with `prefix`.
fn walk(
    raw: &[Raw],
    name_at: &impl Fn(u32) -> String,
    idx: usize,
    prefix: &str,
    out: &mut Vec<Entry>,
) -> Result<()> {
    let dir = raw.get(idx).with_context(|| format!("entry {idx} out of range"))?;
    ensure!(dir.w2 & 0x8000_0000 != 0, "entry {idx} is not a directory");
    let first_child = (dir.w2 & 0x7FFF_FFFF) as usize;
    // the top 4 bits of the child count are flags (set in some audio archives)
    let count = (dir.w3 & 0x0FFF_FFFF) as usize;
    for i in first_child..first_child + count {
        let e = raw.get(i).with_context(|| format!("entry {i} out of range"))?;
        let name = name_at(e.name_off).to_lowercase();
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if e.w2 & 0x8000_0000 != 0 {
            walk(raw, name_at, i, &path, out)?;
        } else {
            out.push(Entry {
                name: path,
                size: e.w1 as u64,
                offset: (e.w2 & 0xFFFF_FF00) as u64,
                kind: (e.w2 & 0xFF) as u8,
                flags: e.w3,
            });
        }
    }
    Ok(())
}
