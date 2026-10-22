//! WAD animation dictionaries: `pgDictionary<crAnimation>`.
//! Layout reverse-engineered by hand and cross-checked against `pc/anim/anim.img` (all quaternion
//! tracks come out unit-length, which is how the format below was validated).
//!
//! dictionary (root)  +0x10 name-hash array · +0x18 item ptr array · +0x1C u16 count
//! animation   +0x08 u16 frames · +0x0A u16 frame limit per sequence · +0x0C f32 duration (s)
//!             +0x14 ptr → sequence ptr array · +0x18 u16 sequence count
//!             +0x1C ptr → name cstring ("pack:/foo.anim") · +0x20 ptr → track descriptor ptr
//!             array (unused for decoding, only for the track count at +0x24) · +0x24 u16 track count
//! sequence    +0x00 ptr → channel ptr array · +0x04 u16 channel count · +0x08 u32 data size
//!             +0x0C u32 frames in this sequence
//! channel     byte0 track type (0 bone translation, 1 bone rotation, 5 mover translation,
//!             6 mover rotation, other = unknown, skip) · byte1 layout (0x10 vector3 from 3 float
//!             sub-channels, 0x11 quaternion from 4 float sub-channels x,y,z,w, 0x01 one quaternion
//!             sub-channel, other = unknown, skip) · u16 @+2 bone id · sub-channel ptrs @+0x04,
//!             +0x08, +0x0C, +0x10 · u32 @+0x14 sub-channel count
//! sub-channel byte @+0x05 encoding: 0x01 raw floats (ptr @+0x08, u16 count @+0x0C), 0x04 static
//!             float (f32 @+0x08), 0x06 quantized float (bitstream ptr @+0x08, u32 bits @+0x0C,
//!             u32 frame count @+0x10, f32 quantum @+0x14, f32 offset @+0x18), 0x09 static
//!             quaternion via pointer (ptr @+0x08 → 4 f32 x,y,z,w), 0x0B static quaternion inline
//!             (4 f32 x,y,z,w @+0x08), other = unsupported (skip the whole channel).

use anyhow::Result;

use crate::rsc::Resource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Translation,
    Rotation,
    MoverTranslation,
    MoverRotation,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub bone_id: u16,
    pub kind: TrackKind,
    /// One value per frame: xyz0 for vectors, xyzw for quaternions.
    pub frames: Vec<[f32; 4]>,
}

/// What a channel was and why it was skipped, if it was.
#[derive(Debug, Clone, Copy)]
pub struct ChannelInfo {
    pub track_type: u8,
    pub layout: u8,
    pub skip_reason: Option<SkipReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    UnknownTrackType,
    UnknownLayout,
    UnsupportedEncoding(u8),
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub name: String,
    pub hash: u32,
    pub frames: u16,
    pub duration: f32,
    pub tracks: Vec<Track>,
    pub skipped_tracks: usize,
    /// Every channel seen (decoded or skipped), for diagnostics.
    pub channels: Vec<ChannelInfo>,
}

fn f32_at(res: &Resource, ptr: u32, field: usize) -> Result<f32> {
    // rsc.rs only exposes integer reads; a resource f32 is just the raw bits of a u32 field.
    Ok(f32::from_bits(res.u32(ptr, field)?))
}

/// Read a scalar-float sub-channel, sampled once per frame in `0..frame_count`.
/// `Ok(Err(enc))` means an encoding we don't understand.
fn float_subchannel(res: &Resource, ptr: u32, frame_count: usize) -> Result<std::result::Result<Vec<f32>, u8>> {
    let enc = res.u8(ptr, 0x05)?;
    match enc {
        0x01 => {
            let data = res.u32(ptr, 0x08)?;
            let count = res.u16(ptr, 0x0C)? as usize;
            let mut vals = Vec::with_capacity(count.max(1));
            for i in 0..count {
                vals.push(f32_at(res, data, i * 4)?);
            }
            if vals.is_empty() {
                vals.push(0.0);
            }
            Ok(Ok((0..frame_count).map(|f| vals[f.min(vals.len() - 1)]).collect()))
        }
        0x04 => {
            let v = f32_at(res, ptr, 0x08)?;
            Ok(Ok(vec![v; frame_count]))
        }
        0x06 => {
            let data = res.u32(ptr, 0x08)?;
            let bits = res.u32(ptr, 0x0C)? as usize;
            let count = res.u32(ptr, 0x10)? as usize;
            let quantum = f32_at(res, ptr, 0x14)?;
            let offset = f32_at(res, ptr, 0x18)?;
            if bits == 0 || count == 0 {
                return Ok(Ok(vec![offset; frame_count]));
            }
            let bytes_needed = (count * bits).div_ceil(8);
            let stream = res.bytes(data, bytes_needed)?;
            let bit = |pos: usize| (stream[pos / 8] >> (pos % 8)) & 1;
            let mut vals = Vec::with_capacity(count);
            for f in 0..count {
                let start = f * bits;
                let mut v: u32 = 0;
                for i in 0..bits {
                    v |= (bit(start + i) as u32) << i;
                }
                vals.push(v as f32 * quantum + offset);
            }
            Ok(Ok((0..frame_count).map(|f| vals[f.min(vals.len() - 1)]).collect()))
        }
        other => Ok(Err(other)),
    }
}

/// Read a whole-quaternion sub-channel (constant across every frame).
fn quat_subchannel(res: &Resource, ptr: u32) -> Result<std::result::Result<[f32; 4], u8>> {
    let enc = res.u8(ptr, 0x05)?;
    match enc {
        0x09 => {
            let q = res.u32(ptr, 0x08)?;
            Ok(Ok([f32_at(res, q, 0)?, f32_at(res, q, 4)?, f32_at(res, q, 8)?, f32_at(res, q, 12)?]))
        }
        0x0B => {
            Ok(Ok([f32_at(res, ptr, 0x08)?, f32_at(res, ptr, 0x0C)?, f32_at(res, ptr, 0x10)?, f32_at(res, ptr, 0x14)?]))
        }
        other => Ok(Err(other)),
    }
}

fn read_channel(res: &Resource, ch: u32, frame_count: usize) -> Result<(Option<Track>, ChannelInfo)> {
    let track_type = res.u8(ch, 0)?;
    let layout = res.u8(ch, 1)?;
    let bone_id = res.u16(ch, 2)?;
    let info = |reason: Option<SkipReason>| ChannelInfo { track_type, layout, skip_reason: reason };

    let kind = match track_type {
        0 => TrackKind::Translation,
        1 => TrackKind::Rotation,
        5 => TrackKind::MoverTranslation,
        6 => TrackKind::MoverRotation,
        _ => return Ok((None, info(Some(SkipReason::UnknownTrackType)))),
    };

    let sub = [res.u32(ch, 0x04)?, res.u32(ch, 0x08)?, res.u32(ch, 0x0C)?, res.u32(ch, 0x10)?];

    let frames = match layout {
        0x10 | 0x11 => {
            let n = if layout == 0x10 { 3 } else { 4 };
            let mut cols = Vec::with_capacity(n);
            for &p in &sub[..n] {
                match float_subchannel(res, p, frame_count)? {
                    Ok(v) => cols.push(v),
                    Err(e) => return Ok((None, info(Some(SkipReason::UnsupportedEncoding(e))))),
                }
            }
            (0..frame_count)
                .map(|f| {
                    let mut v = [0.0f32; 4];
                    for (c, col) in cols.iter().enumerate() {
                        v[c] = col[f];
                    }
                    v
                })
                .collect()
        }
        0x01 => match quat_subchannel(res, sub[0])? {
            Ok(q) => vec![q; frame_count],
            Err(e) => return Ok((None, info(Some(SkipReason::UnsupportedEncoding(e))))),
        },
        _ => return Ok((None, info(Some(SkipReason::UnknownLayout)))),
    };

    Ok((Some(Track { bone_id, kind, frames }), info(None)))
}

fn read_animation(res: &Resource, anim: u32, hash: u32) -> Result<Animation> {
    let frames = res.u16(anim, 0x08)?;
    let duration = f32_at(res, anim, 0x0C)?;
    let seq_list = res.u32(anim, 0x14)?;
    let seq_count = res.u16(anim, 0x18)? as usize;
    let name_ptr = res.u32(anim, 0x1C)?;

    let raw_name = res.cstr(name_ptr)?;
    let name = raw_name.strip_prefix("pack:/").unwrap_or(&raw_name);
    let name = name.strip_suffix(".anim").unwrap_or(name).to_string();

    // Decode every sequence's channels, then concatenate matching channels across sequences
    // (only one sequence has ever been observed, but multi-sequence animations should just work).
    let mut per_seq: Vec<(Vec<Option<Track>>, Vec<ChannelInfo>)> = Vec::with_capacity(seq_count);
    for s in 0..seq_count {
        let seq = res.u32(seq_list, s * 4)?;
        let ch_list = res.u32(seq, 0x00)?;
        let ch_count = res.u16(seq, 0x04)? as usize;
        let seq_frames = res.u32(seq, 0x0C)? as usize;
        let mut tracks = Vec::with_capacity(ch_count);
        let mut infos = Vec::with_capacity(ch_count);
        for c in 0..ch_count {
            let ch = res.u32(ch_list, c * 4)?;
            let (t, info) = read_channel(res, ch, seq_frames)?;
            tracks.push(t);
            infos.push(info);
        }
        per_seq.push((tracks, infos));
    }

    let mut tracks = Vec::new();
    let mut channels = Vec::new();
    if let Some((first_tracks, first_infos)) = per_seq.first() {
        for i in 0..first_tracks.len() {
            channels.push(first_infos[i]);
            let all_present = per_seq.iter().all(|(t, _)| i < t.len() && t[i].is_some());
            if !all_present {
                continue;
            }
            let head = first_tracks[i].as_ref().unwrap();
            let (bone_id, kind) = (head.bone_id, head.kind);
            let mut merged_frames = Vec::new();
            for (t, _) in &per_seq {
                merged_frames.extend_from_slice(&t[i].as_ref().unwrap().frames);
            }
            tracks.push(Track { bone_id, kind, frames: merged_frames });
        }
    }
    let skipped_tracks = channels.iter().filter(|c| c.skip_reason.is_some()).count();

    Ok(Animation { name, hash, frames, duration, tracks, skipped_tracks, channels })
}

/// A .wad file: the animation dictionary sits at the resource root.
pub fn read_dictionary(res: &Resource) -> Result<Vec<Animation>> {
    let root = Resource::ROOT;
    let hashes = res.u32(root, 0x10)?;
    let list = res.u32(root, 0x18)?;
    let count = res.u16(root, 0x1C)? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let ptr = res.u32(list, i * 4)?;
        let hash = res.u32(hashes, i * 4)?;
        out.push(read_animation(res, ptr, hash)?);
    }
    Ok(out)
}
