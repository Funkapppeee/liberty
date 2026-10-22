//! GTA IV audio: the sound archives in `pc/audio/sfx` (RPF3) and the sound banks inside them.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::crypto::Key;
use crate::placement::hash;
use crate::rpf::Rpf;

/// Names for the hashes the audio data uses (archive paths, banks, waves), collected from the
/// game's own audio config in `pc/audio/config`: every word in `waveslots.xml`, `rpf.xml`,
/// `sounds.dat15`, `game.dat16`, `speech.dat`, `categories.dat15` and `curves.dat12`, hashed.
pub fn names(root: &Path) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let dir = root.join("pc/audio/config");
    for file in ["waveslots.xml", "waveslot_.xml", "rpf.xml", "sounds.dat15", "game.dat16", "speech.dat", "categories.dat15", "curves.dat12"] {
        let Ok(data) = std::fs::read(dir.join(file)) else { continue };
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'@' || b == b'-' || b == b'.';
        for w in data.split(|b| !word(*b)).filter(|w| w.len() >= 2) {
            let w = String::from_utf8_lossy(w).into_owned();
            out.entry(hash(&w)).or_insert(w);
        }
    }
    out
}

/// One sound in a bank.
#[derive(Debug, Clone)]
pub struct WaveInfo {
    /// Hash of the wave's name.
    pub hash: u32,
    pub sample_rate: u32,
    /// Bytes of sample data (ADPCM: two samples per byte; PCM: two bytes per sample).
    pub bytes: usize,
    pub samples: usize,
    /// Where the data starts, from the start of the bank file.
    pub offset: usize,
    /// IMA ADPCM, with the decoder state at the start of each 2048-byte block; None = 16-bit PCM.
    pub adpcm: Option<Vec<(i16, u8)>>,
}

impl WaveInfo {
    pub fn seconds(&self) -> f32 {
        self.samples as f32 / self.sample_rate.max(1) as f32
    }
}

const BLOCK: usize = 2048;

fn le<const N: usize>(d: &[u8], at: usize) -> Result<[u8; N]> {
    d.get(at..at + N).and_then(|b| b.try_into().ok()).with_context(|| format!("sound bank cut short at {at:#x}"))
}

/// The waves of a mono sound bank (the files inside `resident.rpf` and friends). Layout: a
/// 28-byte header (i64 wave table offset, i64 other table, i32 wave count, i32, i32 header size);
/// the wave table, 16 bytes per wave (i64 info offset from the table's end, u32 name hash, i32
/// info size); each info (i64 data offset from the header's end, u32 name hash, i32 data bytes,
/// i32 samples, i32, u16 sample rate, u16, and when the info is longer than 32 bytes: i32, i64,
/// u32, u32, u32, i32 block count, then per block i16 predictor + u8 step index).
pub fn read_bank(d: &[u8]) -> Result<Vec<WaveInfo>> {
    let table = i64::from_le_bytes(le(d, 0)?) as usize;
    let count = i32::from_le_bytes(le(d, 16)?) as usize;
    let header_size = i32::from_le_bytes(le(d, 24)?) as usize;
    ensure!(table <= header_size && header_size <= d.len() && count < 100_000, "not a mono sound bank");
    let infos = table + count * 16;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = table + i * 16;
        let at = infos + i64::from_le_bytes(le(d, e)?) as usize;
        let size = i32::from_le_bytes(le(d, e + 12)?) as usize;
        let offset = header_size + i64::from_le_bytes(le(d, at)?) as usize;
        let hash = u32::from_le_bytes(le(d, at + 8)?);
        let bytes = i32::from_le_bytes(le(d, at + 12)?) as usize;
        let samples = i32::from_le_bytes(le(d, at + 16)?) as usize;
        let sample_rate = u16::from_le_bytes(le(d, at + 24)?) as u32;
        let mut adpcm = None;
        if size > 32 {
            let blocks = i32::from_le_bytes(le(d, at + 52)?) as usize;
            if blocks > 0 {
                ensure!(blocks < 1_000_000, "wave {i}: {blocks} ADPCM blocks");
                let states = (0..blocks)
                    .map(|k| Ok((i16::from_le_bytes(le(d, at + 56 + k * 3)?), d[at + 56 + k * 3 + 2])))
                    .collect::<Result<Vec<_>>>()?;
                adpcm = Some(states);
            }
        }
        ensure!(offset + bytes <= d.len(), "wave {i}: data runs past the end of the bank");
        out.push(WaveInfo { hash, sample_rate, bytes, samples, offset, adpcm });
    }
    Ok(out)
}

const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const IMA_STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173,
    190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818,
    18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// A wave's samples, 16-bit mono. ADPCM is standard IMA/DVI: 4 bits a sample, low nibble first,
/// the decoder state reset from the wave's own table at each 2048-byte block.
pub fn decode(d: &[u8], w: &WaveInfo) -> Vec<i16> {
    let data = &d[w.offset..w.offset + w.bytes];
    let Some(states) = &w.adpcm else {
        return data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    };
    let mut out = Vec::with_capacity(data.len() * 2);
    let (mut pred, mut index) = (0i32, 0i32);
    for (k, block) in data.chunks(BLOCK).enumerate() {
        if let Some(&(p, i)) = states.get(k) {
            (pred, index) = (p as i32, (i as i32).clamp(0, 88));
        }
        for &byte in block {
            for nibble in [byte & 0xF, byte >> 4] {
                let step = IMA_STEP[index as usize];
                let mut diff = step >> 3;
                if nibble & 1 != 0 {
                    diff += step >> 2;
                }
                if nibble & 2 != 0 {
                    diff += step >> 1;
                }
                if nibble & 4 != 0 {
                    diff += step;
                }
                pred = (if nibble & 8 != 0 { pred - diff } else { pred + diff }).clamp(-32768, 32767);
                index = (index + IMA_INDEX[nibble as usize]).clamp(0, 88);
                out.push(pred as i16);
            }
        }
    }
    out.truncate(w.samples.max(1).min(out.len()));
    out
}

/// A 16-bit mono WAV file of the samples.
pub fn wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Which sound plays when: `pc/audio/config/sounds.dat15` (sound objects) and `game.dat16` (game
// objects: weapons, footstep surfaces, collision materials, ...).
//
// Both files share a layout: u32 version, u32 data size, the data block; then a string table
// (u32 table size, u32 count, u32 offsets, NUL-terminated strings: in sounds.dat15 the bank
// names, e.g. `RESIDENT\WEAPONS`); then the object index (u32 count, u32 size, and per object a
// u8 name length, the name, u32 offset into the data block, u32 size).
//
// A sound object: u8 type, u32, `AA AA`, u32, u32 field bits, then the fields the bits name in bit
// order (sizes in `FIELD_SIZES`; what each holds in `Sound`), then the type's own data. The sizes
// were worked out from the multitracks (type 13: fields, then u8 count + count x (u32 child hash,
// u32)): with them all 3,075 multitracks in sounds.dat15 come out exactly. Type 12 is a plain wave: its
// last 8 bytes are the hash of the bank path (with `/`) and the hash of the wave name. Every other
// type refers to child sounds by the hash of their names. The exact per-type layouts aren't
// decoded: children are found by scanning the object for hashes of known sound names, and the
// types are read by what the game's own names show they do (see `Sounds::layers`).
// ---------------------------------------------------------------------------------------------

/// The objects of a sounds/game .dat file: (bank or string table, [(name, data)]).
fn read_dat(d: &[u8]) -> Result<(Vec<String>, Vec<(String, &[u8])>)> {
    let u32_at = |at: usize| -> Result<usize> { Ok(u32::from_le_bytes(le(d, at)?) as usize) };
    let size = u32_at(4)?;
    let o = 8 + size;
    let (table_size, count) = (u32_at(o)?, u32_at(o + 4)?);
    let base = o + 8 + 4 * count;
    let mut strings = Vec::with_capacity(count);
    for i in 0..count {
        let at = base + u32_at(o + 8 + 4 * i)?;
        let end = d.get(at..).and_then(|s| s.iter().position(|&b| b == 0)).context("string table cut short")?;
        strings.push(String::from_utf8_lossy(&d[at..at + end]).into_owned());
    }
    let mut p = o + 4 + table_size;
    let n = u32_at(p)?;
    p += 8;
    let mut objects = Vec::with_capacity(n);
    for _ in 0..n {
        let len = *d.get(p).context("object index cut short")? as usize;
        let name = String::from_utf8_lossy(d.get(p + 1..p + 1 + len).context("object index cut short")?).into_owned();
        let (off, sz) = (u32_at(p + 1 + len)?, u32_at(p + 5 + len)?);
        objects.push((name, d.get(8 + off..8 + off + sz).context("object outside the data block")?));
        p += 9 + len;
    }
    Ok((strings, objects))
}

#[derive(Debug, Clone)]
pub struct Sound {
    pub name: String,
    pub kind: u8,
    /// This sound's own volume, dB (0 when it sets none).
    pub volume_db: f32,
    /// Fade-in and fade-out times in ms (field bits 10 and 11), when the sound sets them. E.g. every
    /// car horn: release 50; most sounds set neither.
    pub attack_ms: Option<u16>,
    pub release_ms: Option<u16>,
    /// Its category in categories.dat15 (field bit 13, the hash of the category's name), e.g.
    /// PISTOL_FIRE -> WEAPONS_GUNS, the horns -> VEHICLES_HORNS, the engine loops -> CARS_ENGINES.
    pub category: Option<u32>,
    /// An envelope sound (type 2): its own times around its child.
    pub envelope: Option<Envelope>,
    /// Child sounds, by name hash, in the order they appear.
    pub children: Vec<u32>,
    /// A plain wave (type 12): hash of the bank path, hash of the wave name.
    pub wave: Option<(u32, u32)>,
}

/// An envelope (type 2) after the common fields: u16 attack ms, u16 decay ms, u8 sustain %, i32 hold
/// ms (-1 = until stopped), i32 release ms, then the attack/decay/release curve hashes (curves.dat12:
/// LINEAR_RISE, LINEAR_FALL, ...) and the child. E.g. SIRENS_SLOW_MULTI_SIREN_ENVELOPE: 0, 0, 100,
/// -1, 7500 (a siren dies away over 7.5 s when stopped); GUN_SWEETNERS_SINES_ENV: 0, 0, 100, 25, 50.
#[derive(Debug, Clone, Copy)]
pub struct Envelope {
    pub attack_ms: u16,
    pub decay_ms: u16,
    pub sustain: u8,
    pub hold_ms: i32,
    pub release_ms: i32,
}

/// Bytes of each header field, by field bit (0 = size not known: a sound using one is read no
/// further). 0 volume i16 1/100 dB, 1 volume variance, 2 pitch i16 cents, 3 pitch variance, 4 pan
/// (degrees: 270/90 = left/right), 5 pan variance, 6 pre-delay ms, 7 its variance, 8/9 start offset +
/// variance (u32), 10 attack ms, 11 release ms, 12 (0 on 1,012 sounds, 30 on the horns: likely the
/// doppler factor), 13 category hash, 15 (u16), 16/17 (u8), 18-20 and 23 (u32: game-variable hashes
/// and a float, on the ambient machinery LFOs). Names for 0-13 follow the values the game stores.
const FIELD_SIZES: [u8; 24] = [2, 2, 2, 2, 2, 2, 2, 2, 4, 4, 2, 2, 2, 4, 0, 2, 1, 1, 4, 4, 4, 0, 0, 4];

/// The fields a sound sets, as (bit, raw value), and where its own data starts.
fn header_fields(b: &[u8]) -> (Vec<(usize, u32)>, usize) {
    let mut out = Vec::new();
    let bits = u32::from_le_bytes([b[11], b[12], b[13], b[14]]);
    let mut p = 15;
    for (bit, &size) in FIELD_SIZES.iter().enumerate() {
        if bits >> bit & 1 == 0 {
            continue;
        }
        let size = size as usize;
        if size == 0 || p + size > b.len() {
            return (out, p);
        }
        let mut v = [0u8; 4];
        v[..size].copy_from_slice(&b[p..p + size]);
        out.push((bit, u32::from_le_bytes(v)));
        p += size;
    }
    (out, p)
}

/// The game's own distance rolloff, `DEFAULT_ROLLOFF` in curves.dat12 (curve type 13, input range
/// 0..128). The curve file holds only its range; the shape is fixed in the game (GTAIV.exe 0x8acef0,
/// type 13): flat to 4.99 m, then a degree-8 polynomial in dB over t = (d - 63.95) x 0.027052762, and
/// silent (-100 dB) from 128 m. -0.4 dB at 5 m, -14 at 10, -31 at 20, -55 at 50, -84 at 127. The
/// distance going in is already divided by the sound's rolloff scale (`Categories::rolloff`).
pub fn default_rolloff_db(distance: f32) -> f32 {
    const C: [f32; 9] = [-62.4395, -17.0421, 4.3724, -2.5018, 0.715, 0.9942, -0.2507, -0.5388, 0.2201];
    if distance < 4.99 {
        return 0.0;
    }
    if distance >= 128.0 {
        return -100.0;
    }
    let t = (distance - 63.95) * 0.027_052_762;
    C.iter().rev().fold(0.0, |acc, c| acc * t + c)
}

/// `pc/audio/config/categories.dat15`: the category tree (BASE at the top). A category: u8, u32,
/// `AA 02`?, ..., i16 volume at 11, LPF cutoff at 17 (23900), then six u16 percentages from 21, u8
/// child count at 35 and the children's name hashes. The first percentage reads as the DISTANCE
/// ROLLOFF SCALE (inferred from its values: footsteps 75, guns 300, horns 300, sirens 400, every
/// plain vehicle category 100, BASE 300), and scales multiply down the tree (inferred).
pub struct Categories {
    /// category -> (parent, own rolloff scale)
    cats: HashMap<u32, (Option<u32>, f32)>,
}

impl Categories {
    pub fn load(root: &Path) -> Result<Categories> {
        let data = std::fs::read(root.join("pc/audio/config/categories.dat15")).context("pc/audio/config/categories.dat15")?;
        let (_, objects) = read_dat(&data)?;
        let mut parent = HashMap::new();
        let mut own = HashMap::new();
        for (name, b) in &objects {
            if b.len() < 36 {
                continue;
            }
            own.insert(hash(name), u16::from_le_bytes([b[21], b[22]]) as f32 / 100.0);
            for k in 0..b[35] as usize {
                if let Some(c) = b.get(36 + 4 * k..40 + 4 * k) {
                    parent.insert(u32::from_le_bytes(c.try_into().unwrap()), hash(name));
                }
            }
        }
        Ok(Categories { cats: own.into_iter().map(|(c, r)| (c, (parent.get(&c).copied(), r))).collect() })
    }

    /// A category's rolloff scale: its own times its parents' up to BASE (1 for an unknown one).
    pub fn rolloff(&self, category: u32) -> f32 {
        let (mut c, mut scale) = (Some(category), 1.0);
        for _ in 0..16 {
            let Some((parent, own)) = c.and_then(|c| self.cats.get(&c)) else { break };
            scale *= own;
            c = *parent;
        }
        scale
    }
}

/// One wave to play as part of a sound.
#[derive(Debug, Clone, Copy)]
pub struct Layer {
    pub bank: u32,
    pub wave: u32,
    /// Linear gain from the volumes on the way down the tree.
    pub gain: f32,
    /// -1 left, 0 centre, 1 right.
    pub pan: f32,
}

pub struct Sounds {
    /// Bank paths as the game writes them (`RESIDENT\WEAPONS`), by the hash sounds refer to them with.
    pub banks: HashMap<u32, String>,
    pub sounds: HashMap<u32, Sound>,
}

/// Sound types, named for what the game's own sound names show them doing.
pub const KIND_WAVE: u8 = 12;
/// Plays every child together (e.g. PISTOL_FIRE: shot, lows, tail, sweeteners).
pub const KIND_MULTITRACK: u8 = 13;
/// Picks one child (e.g. GUN_SWEETNERS_SMACK_SOUNDS: SMACK_SOUND_1..5).
pub const KIND_RANDOM: u8 = 14;
/// A left and a right child (e.g. PISTOL_SHOT_1: ..._SHOT1_L, ..._SHOT1_R).
pub const KIND_STEREO: u8 = 11;
/// Near sound, then the distant version (e.g. XFADE_GUN_TEST_PISTOL: PISTOL_FIRE, DISTANT_PISTOL).
pub const KIND_CROSSFADE: u8 = 10;
/// Children one after another (e.g. SHELL_CASINGS_METAL_BOUNCE_1).
pub const KIND_SEQUENCE: u8 = 7;
/// A list of alternatives (e.g. ..._SHORT_BLAST_MT_HORN: DISTANT_CAR_HORNS_01..07).
pub const KIND_SWITCH: u8 = 16;
/// Attack / decay / sustain / hold / release around one child (e.g. SIRENS_SLOW_MULTI_SIREN_ENVELOPE).
pub const KIND_ENVELOPE: u8 = 2;

impl Sounds {
    pub fn load(root: &Path) -> Result<Sounds> {
        let data = std::fs::read(root.join("pc/audio/config/sounds.dat15")).context("pc/audio/config/sounds.dat15")?;
        let (bank_names, objects) = read_dat(&data)?;
        let known: std::collections::HashSet<u32> = objects.iter().map(|(n, _)| hash(n)).collect();
        let mut sounds = HashMap::with_capacity(objects.len());
        for (name, b) in &objects {
            let own = hash(name);
            let kind = b.first().copied().unwrap_or(0);
            let framed = b.len() >= 15 && b[5] == 0xAA && b[6] == 0xAA;
            let (fields, data_at) = if framed { header_fields(b) } else { (Vec::new(), 5) };
            let field = |bit: usize| fields.iter().find(|f| f.0 == bit).map(|f| f.1);
            let volume_db = field(0).map_or(0.0, |v| v as u16 as i16 as f32 / 100.0);
            let envelope = (kind == KIND_ENVELOPE && b.len() >= data_at + 13).then(|| {
                let i32_at = |at: usize| i32::from_le_bytes(b[at..at + 4].try_into().unwrap());
                Envelope {
                    attack_ms: u16::from_le_bytes([b[data_at], b[data_at + 1]]),
                    decay_ms: u16::from_le_bytes([b[data_at + 2], b[data_at + 3]]),
                    sustain: b[data_at + 4],
                    hold_ms: i32_at(data_at + 5),
                    release_ms: i32_at(data_at + 9),
                }
            });
            let mut wave = None;
            let mut children = Vec::new();
            if kind == KIND_WAVE && b.len() >= 8 {
                let at = b.len() - 8;
                wave = Some((u32::from_le_bytes(b[at..at + 4].try_into().unwrap()), u32::from_le_bytes(b[at + 4..].try_into().unwrap())));
            } else {
                let mut i = if framed { 15 } else { 5 };
                while i + 4 <= b.len() {
                    let h = u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
                    if h != own && known.contains(&h) {
                        children.push(h);
                        i += 4;
                    } else {
                        i += 1;
                    }
                }
            }
            let (attack_ms, release_ms, category) = (field(10).map(|v| v as u16), field(11).map(|v| v as u16), field(13));
            sounds.insert(own, Sound { name: name.clone(), kind, volume_db, attack_ms, release_ms, category, envelope, children, wave });
        }
        let banks = bank_names.into_iter().map(|b| (hash(&b.replace('\\', "/")), b)).collect();
        Ok(Sounds { banks, sounds })
    }

    /// The waves that make up one playing of a sound: walk the tree, every child of a multitrack,
    /// one child of a random sound (`pick` returns 0..1), the near side of a crossfade, the first
    /// step of a sequence; volumes add up in dB on the way down.
    /// The first value `get` finds walking down a sound's tree from the top (a child inherits what
    /// its parents set): the category, attack or release a playing of the sound runs with.
    pub fn inherited<T>(&self, sound: u32, get: &dyn Fn(&Sound) -> Option<T>) -> Option<T> {
        fn go<T>(s: &Sounds, id: u32, get: &dyn Fn(&Sound) -> Option<T>, depth: u32) -> Option<T> {
            let snd = s.sounds.get(&id).filter(|_| depth < 8)?;
            get(snd).or_else(|| snd.children.iter().find_map(|&c| go(s, c, get, depth + 1)))
        }
        go(self, sound, get, 0)
    }

    /// Fade-in and fade-out of a playing of the sound, seconds: its own attack / release fields, or
    /// an envelope's in its tree (0 when nothing sets them: the game then starts and stops it as is).
    pub fn attack_release(&self, sound: u32) -> (f32, f32) {
        let attack = self.inherited(sound, &|s| s.attack_ms.map(|v| v as f32).or(s.envelope.map(|e| e.attack_ms as f32)));
        let release = self.inherited(sound, &|s| s.release_ms.map(|v| v as f32).or(s.envelope.map(|e| e.release_ms.max(0) as f32)));
        (attack.unwrap_or(0.0) / 1000.0, release.unwrap_or(0.0) / 1000.0)
    }

    pub fn layers(&self, sound: u32, pick: &mut dyn FnMut() -> f32) -> Vec<Layer> {
        let mut out = Vec::new();
        self.walk(sound, 0.0, 0.0, 0, pick, &mut out);
        out
    }

    fn walk(&self, sound: u32, db: f32, pan: f32, depth: u32, pick: &mut dyn FnMut() -> f32, out: &mut Vec<Layer>) {
        let Some(s) = self.sounds.get(&sound) else { return };
        if depth > 12 || s.name == "NULL_SOUND" {
            return;
        }
        let db = db + s.volume_db;
        if let Some((bank, wave)) = s.wave {
            out.push(Layer { bank, wave, gain: 10f32.powf(db / 20.0), pan });
            return;
        }
        match s.kind {
            KIND_RANDOM | KIND_SWITCH if !s.children.is_empty() => {
                let i = ((pick() * s.children.len() as f32) as usize).min(s.children.len() - 1);
                self.walk(s.children[i], db, pan, depth + 1, pick, out);
            }
            KIND_CROSSFADE | KIND_SEQUENCE => {
                // the first child that makes a sound (a sequence can open with a logic node)
                for &c in &s.children {
                    let before = out.len();
                    self.walk(c, db, pan, depth + 1, pick, out);
                    if out.len() > before {
                        break;
                    }
                }
            }
            KIND_STEREO if s.children.len() == 2 => {
                self.walk(s.children[0], db, -1.0, depth + 1, pick, out);
                self.walk(s.children[1], db, 1.0, depth + 1, pick, out);
            }
            _ => {
                for &c in &s.children {
                    self.walk(c, db, pan, depth + 1, pick, out);
                }
            }
        }
    }
}

/// `game.dat16`: each game object's sound references, in the order they appear in the object
/// (a weapon model's first is its fire sound; a footstep surface lists heel, toe, scuff, run and
/// jump-landing for each shoe type). NULL_SOUND entries are kept, so positions hold.
pub fn game_objects(root: &Path, sounds: &Sounds) -> Result<HashMap<String, Vec<u32>>> {
    let data = std::fs::read(root.join("pc/audio/config/game.dat16")).context("pc/audio/config/game.dat16")?;
    let (_, objects) = read_dat(&data)?;
    let mut out = HashMap::with_capacity(objects.len());
    for (name, b) in objects {
        let mut refs = Vec::new();
        let mut i = 5;
        while i + 4 <= b.len() {
            let h = u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
            if sounds.sounds.contains_key(&h) {
                refs.push(h);
                i += 4;
            } else {
                i += 1;
            }
        }
        out.insert(name.to_ascii_uppercase(), refs);
    }
    Ok(out)
}

/// The sound banks, loaded from the archives as they're asked for.
pub struct Banks {
    root: std::path::PathBuf,
    key: Key,
    names: HashMap<u32, String>,
    archives: HashMap<String, Option<Rpf>>,
    /// By bank hash: the bank file and its waves by name hash.
    loaded: HashMap<u32, Option<(Vec<u8>, HashMap<u32, WaveInfo>)>>,
}

impl Banks {
    pub fn new(root: &Path, key: &Key) -> Banks {
        Banks { root: root.to_owned(), key: key.clone(), names: names(root), archives: HashMap::new(), loaded: HashMap::new() }
    }

    /// A wave's samples (16-bit mono) and sample rate. `bank_path` is the game's own name for the
    /// bank (`RESIDENT\WEAPONS`): the first part is the archive in `pc/audio/sfx`.
    pub fn wave(&mut self, bank: u32, bank_path: &str, wave: u32) -> Option<(Vec<i16>, u32)> {
        if !self.loaded.contains_key(&bank) {
            let loaded = self.load(bank_path);
            self.loaded.insert(bank, loaded);
        }
        let (data, waves) = self.loaded.get(&bank)?.as_ref()?;
        let w = waves.get(&wave)?;
        Some((decode(data, w), w.sample_rate))
    }

    fn load(&mut self, bank_path: &str) -> Option<(Vec<u8>, HashMap<u32, WaveInfo>)> {
        let path = bank_path.replace('\\', "/").to_ascii_lowercase();
        let archive = path.split('/').next()?.to_string();
        if !self.archives.contains_key(&archive) {
            let rpf = Rpf::open(&self.root.join(format!("pc/audio/sfx/{archive}.rpf")), &self.key).ok().map(|mut r| {
                r.resolve_names(&self.names);
                r
            });
            self.archives.insert(archive.clone(), rpf);
        }
        let rpf = self.archives.get(&archive)?.as_ref()?;
        let data = rpf.read(rpf.entries.iter().find(|e| e.name == path)?).ok()?;
        let waves = read_bank(&data).ok()?.into_iter().map(|w| (w.hash, w)).collect();
        Some((data, waves))
    }
}

/// Mix the layers of one sound into 16-bit stereo at 44.1 kHz (interleaved L, R). Layers at other
/// rates are resampled (linear). Returns None when no layer's wave was found.
pub fn mix(layers: &[Layer], sounds: &Sounds, banks: &mut Banks) -> Option<Vec<i16>> {
    const RATE: f32 = 44_100.0;
    let mut acc: Vec<[f32; 2]> = Vec::new();
    let mut any = false;
    for l in layers {
        let Some(path) = sounds.banks.get(&l.bank) else { continue };
        let Some((pcm, rate)) = banks.wave(l.bank, path, l.wave) else { continue };
        if pcm.is_empty() || rate == 0 {
            continue;
        }
        any = true;
        let n = (pcm.len() as f32 * RATE / rate as f32) as usize;
        if acc.len() < n {
            acc.resize(n, [0.0; 2]);
        }
        // equal-power-ish pan: centre goes to both at full level, like the game's mono sounds
        let (gl, gr) = (l.gain * (1.0 - l.pan.max(0.0)), l.gain * (1.0 + l.pan.min(0.0)));
        for (i, frame) in acc.iter_mut().enumerate().take(n) {
            let x = i as f32 * rate as f32 / RATE;
            let (k, f) = (x as usize, x.fract());
            let a = pcm[k.min(pcm.len() - 1)] as f32;
            let b = pcm[(k + 1).min(pcm.len() - 1)] as f32;
            let s = a + (b - a) * f;
            frame[0] += s * gl;
            frame[1] += s * gr;
        }
    }
    // layers that add up past full scale are turned down together (the balance between them
    // stays), instead of clipping
    let peak = acc.iter().flat_map(|f| [f[0].abs(), f[1].abs()]).fold(0.0f32, f32::max);
    let scale = if peak > 32_000.0 { 32_000.0 / peak } else { 1.0 };
    any.then(|| acc.iter().flat_map(|f| [(f[0] * scale) as i16, (f[1] * scale) as i16]).collect())
}

/// A 16-bit stereo 44.1 kHz WAV file of interleaved samples.
pub fn wav_stereo(samples: &[i16]) -> Vec<u8> {
    let mut out = wav(samples, 44_100);
    out[22..24].copy_from_slice(&2u16.to_le_bytes());
    out[28..32].copy_from_slice(&(44_100u32 * 4).to_le_bytes());
    out[32..34].copy_from_slice(&4u16.to_le_bytes());
    out
}

/// An ambient rule (game.dat16 type 20): a sound heard now and then from somewhere around.
#[derive(Debug, Clone)]
pub struct AmbientRule {
    pub name: String,
    /// 0 switches the rule off (the distant-shooting rules ship that way).
    pub weight: f32,
    /// How far away it's placed, metres.
    pub min_dist: f32,
    pub max_dist: f32,
    /// Hours of the day it plays in, start..end.
    pub start_hour: u8,
    pub end_hour: u8,
    /// Seconds between playings (read as that; the game's exact use isn't decoded).
    pub interval: f32,
    pub sound: u32,
}

/// An ambient zone (game.dat16 type 19): a rectangle of the map (GTA x, y) and its rules.
#[derive(Debug, Clone)]
pub struct AmbientZone {
    pub name: String,
    pub min: [f32; 2],
    pub max: [f32; 2],
    pub rules: Vec<u32>,
}

/// The ambient zones and rules of game.dat16. Zone: floats at +10 (one unread, then x, y of two
/// corners, one unread), rule count at +35, rule name hashes from +36. Rule: f32 weight +10,
/// f32 min and max distance +14 / +18, u8 start and end hour +22 / +23, u16 interval +24, u32
/// sound name hash +26.
pub fn ambience(root: &Path) -> Result<(Vec<AmbientZone>, HashMap<u32, AmbientRule>)> {
    let data = std::fs::read(root.join("pc/audio/config/game.dat16")).context("pc/audio/config/game.dat16")?;
    let (_, objects) = read_dat(&data)?;
    let f = |b: &[u8], at: usize| f32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    let u = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    let mut zones = Vec::new();
    let mut rules = HashMap::new();
    for (name, b) in objects {
        match b.first() {
            Some(19) if b.len() >= 36 => {
                let (x0, y0, x1, y1) = (f(b, 14), f(b, 18), f(b, 22), f(b, 26));
                let n = (b[35] as usize).min((b.len() - 36) / 4);
                zones.push(AmbientZone {
                    name,
                    min: [x0.min(x1), y0.min(y1)],
                    max: [x0.max(x1), y0.max(y1)],
                    rules: (0..n).map(|i| u(b, 36 + 4 * i)).collect(),
                });
            }
            Some(20) if b.len() >= 30 => {
                rules.insert(
                    hash(&name),
                    AmbientRule {
                        name,
                        weight: f(b, 10),
                        min_dist: f(b, 14),
                        max_dist: f(b, 18),
                        start_hour: b[22],
                        end_hour: b[23],
                        interval: u16::from_le_bytes([b[24], b[25]]) as f32,
                        sound: u(b, 26),
                    },
                );
            }
            _ => {}
        }
    }
    Ok((zones, rules))
}

#[cfg(test)]
mod rolloff_tests {
    use super::default_rolloff_db;

    /// The polynomial meets the flat part at 5 m and falls steadily to the 128 m cut-off.
    #[test]
    fn default_rolloff_shape() {
        assert!(default_rolloff_db(5.0).abs() < 0.6);
        assert!((default_rolloff_db(10.0) + 14.1).abs() < 0.2);
        assert!((default_rolloff_db(50.0) + 55.2).abs() < 0.2);
        assert_eq!(default_rolloff_db(128.0), -100.0);
        let mut last = 0.0;
        for d in 5..128 {
            let db = default_rolloff_db(d as f32);
            assert!(db <= last + 1e-3, "rises at {d} m");
            last = db;
        }
    }
}
