//! GTA IV archive encryption: AES-256-ECB, each 16-byte block decrypted 16 times.
//!
//! The key is read at runtime from the user's own `GTAIV.exe` and checked against the public
//! SHA1 fingerprint (gtamods.com/wiki/Cryptography). It is never printed or written anywhere,
//! which is why [`Key`] deliberately has no `Debug` impl.

use std::path::Path;

use aes::Aes256;
use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use anyhow::{Context, Result, bail};
use sha1::{Digest, Sha1};

/// SHA1 of the key — a fingerprint to recognise it, not the key itself.
const KEY_SHA1: [u8; 20] = [
    0xDE, 0xA3, 0x75, 0xEF, 0x1E, 0x6E, 0xF2, 0x22, 0x3A, 0x12, 0x21, 0xC2, 0xC5, 0x75, 0xC4, 0x7B,
    0xF1, 0x7E, 0xFA, 0x5E,
];

/// Where the key sits in known exe builds; 0xC5B73C is 1.2.0.59 (Steam Complete Edition).
const KNOWN_OFFSETS: &[usize] = &[
    0xC5B73C, 0xC5B33C, 0xC95FD8, 0xBE7540, 0xBE6540, 0xB7AEF4, 0xB75C9C, 0xB56BC4, 0xB607C4,
    0xA94204, 0xC705E0, 0xBEF028,
];

#[derive(Clone)]
pub struct Key(Aes256);

impl Key {
    /// Find the key inside `GTAIV.exe`: known offsets first, then a full scan.
    pub fn from_exe(exe: &Path) -> Result<Key> {
        let data = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
        let matches = |off: usize| {
            data.get(off..off + 32)
                .is_some_and(|k| Sha1::digest(k).as_slice() == KEY_SHA1)
        };
        let off = KNOWN_OFFSETS
            .iter()
            .copied()
            .find(|&o| matches(o))
            .or_else(|| (0..data.len().saturating_sub(32)).step_by(4).find(|&o| matches(o)));
        match off {
            Some(o) => Ok(Key(Aes256::new(GenericArray::from_slice(&data[o..o + 32])))),
            None => bail!("no GTA IV key found in {}", exe.display()),
        }
    }

    /// A key from raw bytes (for other RAGE games' archives; found by `liberty keyscan`).
    pub fn from_bytes(k: &[u8; 32]) -> Key {
        Key(Aes256::new(GenericArray::from_slice(k)))
    }

    /// Decrypt in place with `rounds` passes per block (GTA IV: 16).
    pub fn decrypt_rounds(&self, buf: &mut [u8], rounds: usize) {
        let whole = buf.len() / 16 * 16;
        for chunk in buf[..whole].chunks_exact_mut(16) {
            let block = GenericArray::from_mut_slice(chunk);
            for _ in 0..rounds {
                self.0.decrypt_block(block);
            }
        }
    }

    /// Decrypt in place. A trailing partial block is stored plain by Rockstar and left alone.
    pub fn decrypt(&self, buf: &mut [u8]) {
        let whole = buf.len() / 16 * 16;
        for chunk in buf[..whole].chunks_exact_mut(16) {
            let block = GenericArray::from_mut_slice(chunk);
            for _ in 0..16 {
                self.0.decrypt_block(block);
            }
        }
    }
}
