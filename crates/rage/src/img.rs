//! IMG v3 archives (GTA IV). Layout verified on 1.2.0.59:
//! - 20-byte header; when encrypted, only bytes 0..16 are (magic, version, count, table size),
//!   bytes 16..20 are plain (entry size = 16, unknown).
//! - Table at offset 20, encrypted as its own block run: `count` 16-byte entries, then
//!   NUL-separated names.
//! - Entry: u32 RSC flags · u32 type · u32 sector · u16 blocks · u16 extra.
//!   Data lives at `sector * 2048`, `blocks * 2048` bytes long.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::crypto::Key;

const MAGIC: u32 = 0xA94E2A52;
const SECTOR: u64 = 2048;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// Resource type from the table (0x01 generic, 0x08 textures, 0x20 bounds, 0x6E model, ...).
    pub kind: u32,
    /// RSC flags copied from the resource header (encodes its memory sizes).
    pub flags: u32,
    pub offset: u64,
    pub size: u64,
}

impl Entry {
    pub fn ext(&self) -> &str {
        self.name.rsplit_once('.').map_or("", |(_, e)| e)
    }
}

pub struct Img {
    pub path: PathBuf,
    pub encrypted: bool,
    pub entries: Vec<Entry>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

impl Img {
    pub fn open(path: &Path, key: &Key) -> Result<Img> {
        let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut head = [0u8; 20];
        f.read_exact(&mut head)?;
        let encrypted = u32_at(&head, 0) != MAGIC;
        if encrypted {
            key.decrypt(&mut head[..16]);
        }
        let (magic, version) = (u32_at(&head, 0), u32_at(&head, 4));
        if magic != MAGIC || version != 3 {
            bail!("{}: not an IMG v3 archive (magic {magic:08X}, version {version})", path.display());
        }
        let count = u32_at(&head, 8) as usize;
        let table_size = u32_at(&head, 12) as usize;
        let item_size = u16_at(&head, 16) as usize;
        ensure!(item_size == 16, "unexpected IMG entry size {item_size}");
        ensure!(count * item_size <= table_size, "IMG table too small for {count} entries");

        let mut table = vec![0u8; table_size];
        f.read_exact(&mut table)?;
        if encrypted {
            key.decrypt(&mut table);
        }
        let names = table[count * item_size..].split(|&b| b == 0);
        let file_len = f.metadata()?.len();
        let entries = (0..count)
            .zip(names)
            .map(|(i, name)| {
                let e = &table[i * item_size..];
                Entry {
                    name: String::from_utf8_lossy(name).into_owned(),
                    flags: u32_at(e, 0),
                    kind: u32_at(e, 4),
                    offset: u32_at(e, 8) as u64 * SECTOR,
                    size: u16_at(e, 12) as u64 * SECTOR,
                }
            })
            .collect::<Vec<_>>();
        if let Some(bad) = entries.iter().find(|e| e.offset + e.size > file_len) {
            bail!("{}: entry {} points past the end of the file", path.display(), bad.name);
        }
        Ok(Img { path: path.to_owned(), encrypted, entries })
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))
    }

    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(entry.offset))?;
        let mut buf = vec![0u8; entry.size as usize];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}
