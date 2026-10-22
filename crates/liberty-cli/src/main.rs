//! liberty — dev tool for checking each format reader against the real game files.
//!
//!   liberty list <archive.img>                     what's inside an archive
//!   liberty shaders <archive.img>                  which shaders the models use, and which carry normal / specular maps
//!   liberty scan                                   open every archive + resource, report failures
//!   liberty textures <archive.img> <x.wtd> <dir>   dump a texture dictionary to PNGs
//!   liberty wtd <file.wtd> <dir>                    dump a loose texture pack (pc/textures/...) to PNGs
//!   liberty navmesh <x> <y>                         the navmesh tile under a GTA position: polygons, pavement, adjacency check
//!   liberty water                                   summarize common/data/water.dat
//!   liberty weapons                                 list every weapon in WeaponInfo.xml with its stats
//!   liberty vehicles                                summarize handling.dat/vehicles.ide/carcols.dat
//!   liberty peds                                    check pedVariations.dat/peds.ide against componentpeds.img
//!   liberty sounds <archive.rpf> <bank> [dir]      list the sounds in an audio bank, optionally export them as WAV
//!   liberty sound <name> [n] [dir]                 a named sound (or a game object's n-th sound): tree, layers, mixed WAV
//!   liberty rpf <archive.rpf>                       list an RPF2 archive, resource-check entries
//!   liberty anims <archive.img> <x.wad>             list an animation dictionary
//!   liberty animscan                                decode every .wad in pc/anim/anim.img, report stats
//!   liberty paths [out.png]                         decode paths.img road/pavement network, render a map
//!
//! Archive paths are relative to the game folder: $LIBERTY_GAME, or the Steam install.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use std::collections::HashMap;
use anyhow::{Context, Result, bail};
use rage::anim;
use rage::drawable;
use rage::wtd::{self, Format, Texture};
use rage::{Img, Key, Resource, Rpf};

const DEFAULT_GAME: &str =
    r"C:\Program Files (x86)\Steam\steamapps\common\Grand Theft Auto IV\GTAIV";

fn game_root() -> PathBuf {
    std::env::var_os("LIBERTY_GAME").map_or_else(|| PathBuf::from(DEFAULT_GAME), PathBuf::from)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = game_root();
    let key = Key::from_exe(&root.join("GTAIV.exe"))?;
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["list", img] => list(&Img::open(&root.join(img), &key)?),
        // a loose texture pack (pc/textures/hud.wtd ...): list it, and dump it to PNGs
        ["wtd", file, dir] => {
            let res = Resource::load(&std::fs::read(root.join(file))?)?;
            std::fs::create_dir_all(dir)?;
            for t in wtd::read(&res)? {
                image::save_buffer(Path::new(dir).join(format!("{}.png", t.name)), &decode(&t)?, t.width as u32, t.height as u32, image::ColorType::Rgba8)?;
                println!("  {:32} {:>4}x{:<4} {:?} ({} mips)", t.name, t.width, t.height, t.format, t.levels);
            }
            Ok(())
        }
        // the navmesh tile under a GTA position: polygons, how many are pavement, and whether each
        // edge's neighbour is where it says (same tile: in range; and it lists us back)
        ["navmesh", x, y] => {
            let (x, y): (f32, f32) = (x.parse()?, y.parse()?);
            let area = rage::navmesh::area_at(x, y).context("off the map")?;
            let img = Img::open(&root.join("pc/data/cdimages/navmeshes.img"), &key)?;
            let name = rage::navmesh::tile_name(area);
            let e = img.find(&name).with_context(|| format!("{name} not in navmeshes.img"))?;
            let m = rage::navmesh::read(&Resource::load(&img.read(e)?)?)?;
            let pav = m.polys.iter().filter(|p| p.pavement()).count();
            let (mut same, mut ok, mut back, mut other) = (0, 0, 0, 0);
            for (i, p) in m.polys.iter().enumerate() {
                for &(a, q) in &p.across {
                    if a != m.area {
                        other += 1;
                        continue;
                    }
                    same += 1;
                    if let Some(n) = m.polys.get(q as usize) {
                        ok += 1;
                        back += n.across.iter().any(|&(a2, q2)| a2 == m.area && q2 as usize == i) as usize;
                    }
                }
            }
            println!("{name}: area {} (expected {area}), min {:?}, size {:?}", m.area, m.min, m.size);
            println!("  {} polygons, {pav} pavement", m.polys.len());
            println!("  edges: {same} to this tile ({ok} in range, {back} listed back by the neighbour), {other} to other tiles or none");
            Ok(())
        }
        // every polygon of the tile under a GTA position, raw: index, flags, corners, its 40 bytes,
        // and per edge the neighbour plus the adjacency record's second word (research dump)
        ["navdump", x, y] => {
            let (x, y): (f32, f32) = (x.parse()?, y.parse()?);
            let area = rage::navmesh::area_at(x, y).context("off the map")?;
            let img = Img::open(&root.join("pc/data/cdimages/navmeshes.img"), &key)?;
            let e = img.find(&rage::navmesh::tile_name(area)).context("tile not in navmeshes.img")?;
            let m = rage::navmesh::read(&Resource::load(&img.read(e)?)?)?;
            for (i, p) in m.polys.iter().enumerate() {
                let c = p.centre();
                let raw: String = p.raw.iter().map(|b| format!("{b:02x}")).collect();
                let corners: Vec<String> = p.corners.iter().map(|q| format!("{:.2},{:.2},{:.2}", q[0], q[1], q[2])).collect();
                let edges: Vec<String> = p.across.iter().zip(&p.link).map(|(a, l)| format!("{}:{}:{l:08x}", a.0, a.1)).collect();
                println!("{}	{i}	{:04x}	{:.2}	{:.2}	{:.2}	{raw}	{}	{}", m.area, p.flags, c[0], c[1], c[2], corners.join(" "), edges.join(" "));
            }
            Ok(())
        }
        ["shaders", img] => shader_stats(&Img::open(&root.join(img), &key)?),
        ["scan"] => scan(&root, &key),
        ["textures", img, name, dir] => {
            dump_textures(&Img::open(&root.join(img), &key)?, name, Path::new(dir))
        }
        ["raw", img, name, dir] => dump_raw(&Img::open(&root.join(img), &key)?, name, Path::new(dir)),
        ["model", img, name] => show_model(&Img::open(&root.join(img), &key)?, name),
        ["missing", chunk] => missing_models(&root, &key, chunk),
        // a fragment's models: the bytes that say which bone each hangs on, beside the bones
        ["fragmodels", img, name] => {
            let img = Img::open(&root.join(img), &key)?;
            let e = img.find(name).with_context(|| format!("{name} not in archive"))?;
            let res = Resource::load(&img.read(e)?)?;
            let d = res.u32(Resource::ROOT, 0xB4)?;
            for (i, b) in drawable::read_skeleton(&res, d).unwrap_or_default().iter().enumerate() {
                println!("bone {i}: {} id {} parent {:?} at {:.2} {:.2} {:.2} local {:.2} {:.2} {:.2} rot {:.3} {:.3} {:.3} {:.3} orient {:.2} {:.2} {:.2}", b.name, b.id, b.parent, b.abs_pos[0], b.abs_pos[1], b.abs_pos[2], b.local_pos[0], b.local_pos[1], b.local_pos[2], b.local_rot[0], b.local_rot[1], b.local_rot[2], b.local_rot[3], b.orient[0], b.orient[1], b.orient[2]);
            }
            for l in drawable::read_fragment_lights(&res, d) {
                println!("light {:02x}{:02x}{:02x} bone {} corona {:.2} draw {:.0} at {:.2} {:.2} {:.2} dir {:.2} {:.2} {:.2}", l.color[0], l.color[1], l.color[2], l.bone, l.corona, l.draw_dist, l.pos[0], l.pos[1], l.pos[2], l.dir[0], l.dir[1], l.dir[2]);
            }
            let root = Resource::ROOT;
            // the rest matrices every rigid model is placed by (fragType +0x1E4, else from the bones)
            let bones = drawable::read_skeleton(&res, d).unwrap_or_default();
            for (i, m) in drawable::rest_matrices(&res, &bones).iter().enumerate() {
                println!("matrix {i}: {}", m.iter().map(|r| format!("[{:.3} {:.3} {:.3} | {:.3}]", r[0], r[1], r[2], r[3])).collect::<Vec<_>>().join(" "));
            }
            let list = res.u32(root, 0xD4)?;
            for i in 0..res.u8(root, 0x1F3)? as usize {
                let child = res.u32(list, i * 4)?;
                let cd = res.u32(child, 0x90)?;
                let n = if cd != 0 { drawable::read_at(&res, cd).map(|d| d.meshes.len()).unwrap_or(0) } else { 0 };
                let first = if cd != 0 { drawable::read_at(&res, cd).ok().and_then(|d| d.meshes.first().and_then(|m| m.positions.first().copied())) } else { None };
                println!("child {i}: {n} meshes, first vertex {first:?}");
                if cd != 0 {
                    // the child's geometry extent in its own (untransformed) coordinates
                    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                    for p in drawable::read_at(&res, cd)?.meshes.iter().flat_map(|m| m.positions.iter()) {
                        for k in 0..3 {
                            lo[k] = lo[k].min(p[k]);
                            hi[k] = hi[k].max(p[k]);
                        }
                    }
                    println!("    local extent x {:.2}..{:.2} y {:.2}..{:.2} z {:.2}..{:.2}", lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]);
                }
                if cd != 0 {
                    // the child drawable's high-detail models: their header words (bone index etc.)
                    let lod = res.u32(cd, 0x40)?;
                    if lod != 0 {
                        let models = res.u32(lod, 0)?;
                        for m in 0..res.u16(lod, 4)? as usize {
                            let model = res.u32(models, m * 4)?;
                            let w: Vec<String> = (0..0x20).step_by(4).map(|o| format!("{:08x}", res.u32(model, o).unwrap_or(0xdead))).collect();
                            println!("    model {m}: {} -> matrix index {} skinned {}", w.join(" "), res.u8(model, 0x17)?, res.u8(model, 0x19)?);
                        }
                    }
                }
                for row in (0..0xA0).step_by(0x20) {
                    let w: Vec<String> = (row..row + 0x20).step_by(4).map(|o| format!("{:08x}", res.u32(child, o).unwrap_or(0xdead))).collect();
                    println!("    +{row:02x}: {}", w.join(" "));
                }
            }
            Ok(())
        }
        ["near", x, y, r] => near(&root, &key, x.parse()?, y.parse()?, r.parse()?),
        // every placement whose model name contains <text>, per chunk: count + the models used
        ["placed", text] => placed(&root, &key, text),
        // every map fragment: does the child's +0x0C agree with its model's matrix index, does the
        // stored rest matrix set exist / carry turns, do turned bones exist
        ["fragsweep"] => fragsweep(&root, &key),
        // a model file's lights wherever they are: scan for the light structure's first word and
        // say which words of the file point at the list
        ["lightscan", img, name] => {
            let img = Img::open(&root.join(img), &key)?;
            let e = img.find(name).with_context(|| format!("{name} not in archive"))?;
            let res = Resource::load(&img.read(e)?)?;
            let (seg, at) = res.at(Resource::ROOT)?;
            let seg = &seg[..];
            let word = |o: usize| u32::from_le_bytes([seg[o], seg[o + 1], seg[o + 2], seg[o + 3]]);
            let hits: Vec<usize> = (0..seg.len().saturating_sub(4)).step_by(4).filter(|&o| word(o) == 0x0069514C).collect();
            println!("{name}: {} light structures at {:x?} (root at {at:#x})", hits.len(), &hits[..hits.len().min(12)]);
            if let Some(&first) = hits.first() {
                let ptr = 0x5000_0000u32 + first as u32;
                let refs: Vec<usize> = (0..seg.len().saturating_sub(4)).step_by(4).filter(|&o| word(o) == ptr).collect();
                println!("  pointers to the first: at {:x?}", refs);
                for r in refs.iter().take(4) {
                    println!("    around {:#x}: {:08x} {:08x} {:08x} {:08x}", r, word(r.saturating_sub(4)), word(*r), word(r + 4), word(r + 8));
                }
                let f = |o: usize| f32::from_bits(word(o));
                for h in hits.iter().take(4) {
                    println!("  light at {h:#x}: pos {:.2} {:.2} {:.2} dir {:.2} {:.2} {:.2} colour {:02x}{:02x}{:02x} range {:.1} strength {:.1} corona {:.2} cone {:.0} flags {:#x} kind {}",
                        f(h + 4), f(h + 8), f(h + 12), f(h + 16), f(h + 20), f(h + 24), seg[h + 0x28], seg[h + 0x29], seg[h + 0x2A], f(h + 0x38), f(h + 0x3C), f(h + 0x40), f(h + 0x48), word(h + 0x4C), seg[h + 0x5B]);
                    println!("    tail words: {:08x} {:08x} {:08x} {:08x} {:08x} {:08x} {:08x}", word(h + 0x50), word(h + 0x54), word(h + 0x58), word(h + 0x5C), word(h + 0x60), word(h + 0x64), word(h + 0x68));
                }
            }
            Ok(())
        }
        // models whose name contains <part>: the words after the drawable's base (where its lights hang)
        ["lightprobe", part] => {
            let index = rage::index::GameIndex::build(&root, &key)?;
            let mut names: Vec<(&u32, &String)> = index.model_name.iter().filter(|(h, n)| n.contains(part) && index.models.contains_key(h)).collect();
            names.sort_by_key(|x| x.1.clone());
            println!("{} models match", names.len());
            for (h, n) in names.iter().take(60) {
                let (a, e) = index.models[h];
                let img = &index.imgs[a];
                let res = Resource::load(&img.read(&img.entries[e])?)?;
                let w: Vec<String> = (0x60..0xA0).step_by(4).map(|o| format!("{:08x}", res.u32(Resource::ROOT, o).unwrap_or(0xdead))).collect();
                println!("{n}: {}", w.join(" "));
                for o in (0x70..0x90).step_by(4) {
                    let ptr = res.u32(Resource::ROOT, o).unwrap_or(0);
                    let cnt = res.u16(Resource::ROOT, o + 4).unwrap_or(0) as usize;
                    if ptr >> 28 == 5 && cnt > 0 && cnt < 64 {
                        for k in 0..cnt.min(1) {
                            if let Ok(b) = res.bytes(ptr + (k * 0x6C) as u32, 0x6C) {
                                let f: Vec<String> = b.chunks_exact(4).map(|c| { let u = u32::from_le_bytes([c[0], c[1], c[2], c[3]]); let v = f32::from_bits(u); if v.abs() > 1e-4 && v.abs() < 1e5 { format!("{v:.3}") } else { format!("{u:#x}") } }).collect();
                                println!("  +{o:#x} [{k}/{cnt}]: {}", f.join(" "));
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        ["wpl", path] => show_wpl(&std::fs::read(root.join(path))?, path),
        ["wplnames", path] => {
            let index = rage::index::GameIndex::build(&root, &key)?;
            let inst = rage::placement::read_wpl(&std::fs::read(root.join(path))?)?;
            let mut names: BTreeMap<String, usize> = BTreeMap::new();
            for i in &inst {
                let n = index.model_name.get(&i.model_hash).cloned().unwrap_or_else(|| format!("{:08x}", i.model_hash));
                let found = if index.models.contains_key(&i.model_hash) { "" } else { " (no .wdr/.wft)" };
                *names.entry(format!("{n}{found}")).or_default() += 1;
            }
            for (n, c) in names.iter().take(60) {
                println!("  x{c:<3} {n}");
            }
            println!("{} instances, {} distinct", inst.len(), names.len());
            Ok(())
        }
        ["bounds", img, name] => {
            let img = Img::open(&root.join(img), &key)?;
            let e = img.find(name).with_context(|| format!("{name} not in archive"))?;
            let m = rage::bounds::read_wbn(&Resource::load(&img.read(e)?)?)?;
            let (lo, hi) = m.vertices.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(mut lo, mut hi), v| { for k in 0..3 { lo[k] = lo[k].min(v[k]); hi[k] = hi[k].max(v[k]); } (lo, hi) });
            println!("{name}: {} vertices, {} polygons -> {} triangles, {} area mismatches, {} unread bound types, bounds {lo:?}..{hi:?}", m.vertices.len(), m.polygons, m.triangles.len(), m.unmatched, m.skipped);
            Ok(())
        }
        ["water"] => show_water(&root),
        ["weapons"] => show_weapons(&root),
        ["vehicles"] => show_vehicles(&root),
        ["peds"] => show_peds(&root, &key),
        // the ambient zones (game.dat16) around a map point and their rules: sound, hours, distance, interval
        ["ambience", x, y] => {
            let (x, y): (f32, f32) = (x.parse()?, y.parse()?);
            let sounds = rage::audio::Sounds::load(&root)?;
            let (zones, rules) = rage::audio::ambience(&root)?;
            for z in zones.iter().filter(|z| x >= z.min[0] && x <= z.max[0] && y >= z.min[1] && y <= z.max[1]) {
                println!("{} ({:.0},{:.0})-({:.0},{:.0})", z.name, z.min[0], z.min[1], z.max[0], z.max[1]);
                for r in &z.rules {
                    match rules.get(r) {
                        Some(r) => println!("  {} weight {} {}-{} m, {}-{} h, every {} s: {}", r.name, r.weight, r.min_dist, r.max_dist, r.start_hour, r.end_hour, r.interval, sounds.sounds.get(&r.sound).map_or("?", |s| s.name.as_str())),
                        None => println!("  rule {r:08x} (not read)"),
                    }
                }
            }
            Ok(())
        }
        ["wpl", img, name] => {
            let img = Img::open(&root.join(img), &key)?;
            let e = img.find(name).with_context(|| format!("{name} not in archive"))?;
            show_wpl(&img.read(e)?, name)
        }
        // one named sound from sounds.dat15 (or a game object's n-th sound, e.g. sound W_GLOCK 0):
        // its tree, the waves one playing uses, and the mix as a WAV
        ["sound", name, ref rest @ ..] if rest.len() <= 2 => {
            let sounds = rage::audio::Sounds::load(&root)?;
            let game = rage::audio::game_objects(&root, &sounds)?;
            let mut out_dir = None;
            let mut id = rage::placement::hash(name);
            let mut rest = rest.iter();
            if let Some(refs) = game.get(&name.to_ascii_uppercase()) {
                for (i, r) in refs.iter().enumerate() {
                    println!("  {name} sound {i}: {}", sounds.sounds[r].name);
                }
                let n: usize = rest.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                id = *refs.get(n).with_context(|| format!("{name} has no sound {n}"))?;
            }
            if let Some(d) = rest.next() {
                out_dir = Some(d.to_string());
            }
            fn tree(s: &rage::audio::Sounds, id: u32, depth: usize) {
                let Some(x) = s.sounds.get(&id) else { return };
                let what = match x.wave {
                    Some((b, w)) => format!("wave {:08x} in {}", w, s.banks.get(&b).map_or("?", String::as_str)),
                    None => format!("type {}", x.kind),
                };
                println!("{}{} ({what}, {:+.1} dB)", "  ".repeat(depth + 1), x.name, x.volume_db);
                if depth < 8 {
                    for &c in &x.children {
                        tree(s, c, depth + 1);
                    }
                }
            }
            tree(&sounds, id, 0);
            let mut seed = 0x2545_F491u32;
            let mut pick = || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed >> 8) as f32 / (1u32 << 24) as f32
            };
            let layers = sounds.layers(id, &mut pick);
            let mut banks = rage::audio::Banks::new(&root, &key);
            let mixed = rage::audio::mix(&layers, &sounds, &mut banks);
            println!("one playing: {} layers", layers.len());
            match mixed {
                Some(m) => {
                    let peak = m.iter().map(|s| (*s as i32).abs()).max().unwrap_or(0);
                    println!("mixed: {:.2} s stereo, peak {peak}", m.len() as f32 / 2.0 / 44_100.0);
                    if let Some(d) = out_dir {
                        std::fs::create_dir_all(&d)?;
                        let f = Path::new(&d).join(format!("{}.wav", sounds.sounds[&id].name));
                        std::fs::write(&f, rage::audio::wav_stereo(&m))?;
                        println!("wrote {}", f.display());
                    }
                }
                None => println!("no wave found for it"),
            }
            Ok(())
        }
        // the sounds in one bank of an audio archive, e.g. sounds pc/audio/sfx/resident.rpf weapons [out dir]
        ["sounds", path, bank, ref rest @ ..] if rest.len() <= 1 => {
            let mut rpf = Rpf::open(&root.join(path), &key)?;
            let names = rage::audio::names(&root);
            rpf.resolve_names(&names);
            let e = rpf.find(bank).with_context(|| format!("{bank} not in archive"))?;
            let data = rpf.read(e)?;
            let waves = rage::audio::read_bank(&data)?;
            let mut named = 0;
            for w in &waves {
                let name = names.get(&w.hash).cloned().unwrap_or_else(|| format!("0x{:08x}", w.hash));
                named += names.contains_key(&w.hash) as usize;
                let pcm = rage::audio::decode(&data, w);
                let peak = pcm.iter().map(|s| (*s as i32).abs()).max().unwrap_or(0);
                // the biggest jump between neighbouring samples across a block edge vs anywhere: a
                // decoder that mishandles block states shows up as clicks at the edges
                println!("  {name:40} {:>6} Hz {:>6.2} s  {}  peak {peak:>5}", w.sample_rate, w.seconds(), if w.adpcm.is_some() { "adpcm" } else { "pcm  " });
                if let Some(dir) = rest.first() {
                    std::fs::create_dir_all(dir)?;
                    std::fs::write(Path::new(dir).join(format!("{name}.wav")), rage::audio::wav(&pcm, w.sample_rate))?;
                }
            }
            println!("{bank}: {} sounds, {named} with names", waves.len());
            Ok(())
        }
        ["rpf", path] => {
            let mut rpf = Rpf::open(&root.join(path), &key)?;
            if rpf.version == 3 {
                rpf.resolve_names(&rage::audio::names(&root));
            }
            show_rpf(&rpf)
        }
        // a NaturalMotion Euphoria character rig (e.g. RDR's cowboy.art, extracted): its joints
        ["nmart", file] => {
            let d = std::fs::read(file)?;
            for j in rage::nmart::read_biped(&d)? {
                let deg: Vec<String> = j.limits.iter().map(|a| format!("{:6.1}", a.to_degrees())).collect();
                let m: Vec<String> = j.muscle.iter().map(|x| format!("{x}")).collect();
                println!("{:20} {} parts {:?}  limits [{}]  muscle [{}]", j.name, if j.hinge { "hinge " } else { "3-dof " }, j.parts, deg.join(" "), m.join(" "));
                println!("{:20} vectors {:?}", "", j.vectors);
            }
            Ok(())
        }
        // decrypt an RPF6 (Red Dead) header area to a file for study: TOC at 0x10 and the name
        // ("debug") block at u32@8 * 8, with the key at <keyoff> in the game's exe
        ["rpf6raw", exe, keyoff, rpf, out] => {
            let data = std::fs::read(exe)?;
            let off = usize::from_str_radix(keyoff.trim_start_matches("0x"), 16)?;
            let k: &[u8; 32] = data[off..off + 32].try_into()?;
            let key6 = rage::crypto::Key::from_bytes(k);
            let arc = std::fs::read(rpf)?;
            let be = |o: usize| u32::from_be_bytes(arc[o..o + 4].try_into().unwrap());
            let (count, dbg) = (be(4) as usize, be(8) as usize * 8);
            let mut toc = arc[0x10..0x10 + (count * 20).div_ceil(16) * 16].to_vec();
            key6.decrypt_rounds(&mut toc, 16);
            let mut names = arc[dbg.min(arc.len())..].to_vec();
            key6.decrypt_rounds(&mut names, 16);
            println!("{count} entries, debug block at {dbg:#x} ({} bytes)", names.len());
            std::fs::write(format!("{out}.toc"), &toc)?;
            std::fs::write(format!("{out}.names"), &names)?;
            Ok(())
        }
        // find another RAGE game's archive key in its exe: every 4-aligned, high-entropy 32-byte
        // window is tried on the start of an RPF6 TOC (at 0x10); real TOCs are mostly zero bytes.
        // Prints only the offset and a score — never the key.
        ["keyscan", exe, rpf] => {
            let data = std::fs::read(exe)?;
            let arc = std::fs::read(rpf)?;
            let toc = &arc[0x10..0x10 + 256.min(arc.len() - 0x10)];
            let zeros = |b: &[u8]| b.iter().filter(|&&x| x == 0).count();
            let mut best: Vec<(usize, usize, usize)> = Vec::new();
            for off in (0..data.len().saturating_sub(32)).step_by(4) {
                let k: &[u8; 32] = data[off..off + 32].try_into().unwrap();
                let mut seen = [false; 256];
                let distinct = k.iter().filter(|&&b| !std::mem::replace(&mut seen[b as usize], true)).count();
                if distinct < 24 {
                    continue;
                }
                let key = rage::crypto::Key::from_bytes(k);
                for rounds in [1usize, 16] {
                    let mut blk = toc[..16].to_vec();
                    key.decrypt_rounds(&mut blk, rounds);
                    if zeros(&blk) < 5 {
                        continue;
                    }
                    let mut all = toc.to_vec();
                    key.decrypt_rounds(&mut all, rounds);
                    best.push((zeros(&all), off, rounds));
                }
            }
            best.sort_by(|a, b| b.0.cmp(&a.0));
            for (z, off, rounds) in best.iter().take(5) {
                println!("offset {off:#x}  rounds {rounds:>2}  zero bytes {z}/{}", toc.len());
            }
            Ok(())
        }
        // a ped's ragdoll body from its .wft: parts, bones, shapes, masses
        // optional 4th arg: write the parts (name, bone, mass, 4x4 matrix) as JSON for the RE harness
        ["ragdoll", path, name, ref rest @ ..] if rest.len() <= 1 => {
            let bytes = if path.ends_with(".rpf") {
                let rpf = Rpf::open(&root.join(path), &key)?;
                rpf.read(rpf.find(name).with_context(|| format!("{name} not in archive"))?)?
            } else {
                let img = Img::open(&root.join(path), &key)?;
                img.read(img.find(name).with_context(|| format!("{name} not in archive"))?)?
            };
            let res = rage::Resource::load(&bytes)?;
            let body = rage::ragdoll::read_ragdoll(&res)?;
            let sum: f32 = body.parts.iter().map(|p| p.mass).sum();
            println!("{name}: {:.1} kg (parts sum {sum:.1}), inertia {:?}, {} parts", body.mass, body.inertia, body.parts.len());
            let skel = rage::drawable::read_skeleton(&res, res.u32(rage::Resource::ROOT, 0xB4)?)?;
            for (i, p) in body.parts.iter().enumerate() {
                let shape = match p.shape {
                    rage::ragdoll::Shape::Capsule { radius, half_length } => format!("capsule r {radius:.3} len {:.3}", half_length * 2.0),
                    rage::ragdoll::Shape::Box { half } => format!("box {:.3} x {:.3} x {:.3}", half[0] * 2.0, half[1] * 2.0, half[2] * 2.0),
                    rage::ragdoll::Shape::Other(t) => format!("type {t}"),
                };
                let t = p.matrix[3];
                println!("  #{i:<2} bone {:<3} {:<18} {:5.2} kg  {shape:<34} at ({:.2}, {:.2}, {:.2})", p.bone, p.name, p.mass, t[0], t[1], t[2]);
                if let Some(b) = skel.get(p.bone as usize) {
                    let d = |v: [f32; 3]| format!("{:>5.0} {:>5.0} {:>5.0}", v[0].to_degrees(), v[1].to_degrees(), v[2].to_degrees());
                    println!("        limits min [{}]  max [{}]  raw {:08x} {:08x} {:08x} {:08x}", d(b.rot_min), d(b.rot_max), b.raw[0], b.raw[1], b.raw[2], b.raw[3]);
                }
            }
            if let Some(out) = rest.first() {
                let parts: Vec<String> = body.parts.iter().map(|p| {
                    format!("{{\"name\":\"{}\",\"bone\":{},\"mass\":{},\"matrix\":{:?}}}", p.name, p.bone, p.mass, p.matrix)
                }).collect();
                std::fs::write(out, format!("[\n{}\n]\n", parts.join(",\n")))?;
                println!("wrote {out}");
            }
            Ok(())
        }
        // dump a resource object as dwords: floats, and what each pointer points at (reverse engineering)
        ["objdump", path, name, at, len] => {
            let bytes = if path.ends_with(".rpf") {
                let rpf = Rpf::open(&root.join(path), &key)?;
                rpf.read(rpf.find(name).with_context(|| format!("{name} not in archive"))?)?
            } else {
                let img = Img::open(&root.join(path), &key)?;
                img.read(img.find(name).with_context(|| format!("{name} not in archive"))?)?
            };
            let res = rage::Resource::load(&bytes)?;
            let at = u32::from_str_radix(at.trim_start_matches("0x"), 16)?;
            let len = usize::from_str_radix(len.trim_start_matches("0x"), 16)?;
            for o in (0..len).step_by(4) {
                let Ok(v) = res.u32(at, o) else { break };
                let f = f32::from_bits(v);
                let what = if (0x5000_0000..0x7000_0000).contains(&v) {
                    match res.bytes(v, 16) {
                        Ok(b) => format!("-> {}", b.chunks(4).map(|w| format!("{:08x}", u32::from_le_bytes(w.try_into().unwrap()))).collect::<Vec<_>>().join(" ")),
                        Err(_) => "-> (bad ptr)".into(),
                    }
                } else if f.is_finite() && f.abs() > 1e-4 && f.abs() < 1e5 {
                    format!("f {f:.4}")
                } else {
                    String::new()
                };
                println!("+{o:03x}  {v:08x}  {what}");
            }
            Ok(())
        }
        // extract one file from an RPF (decrypting it too if the raw bytes don't look like text/RSC)
        ["rpfget", path, name, out] => {
            let rpf = Rpf::open(&root.join(path), &key)?;
            let e = rpf.find(name).with_context(|| format!("{name} not in archive"))?;
            let mut b = rpf.read(e)?;
            let texty = |b: &[u8]| b.iter().take(256).all(|&c| c == 9 || c == 10 || c == 13 || (32..127).contains(&c));
            println!("{}: {} bytes, flags {:#x}, raw looks like text: {}", e.name, b.len(), e.flags, texty(&b));
            if rpf.encrypted && !texty(&b) {
                let whole = b.len() / 16 * 16;
                key.decrypt(&mut b[..whole]);
                println!("decrypted: looks like text: {}", texty(&b));
            }
            std::fs::write(out, &b)?;
            Ok(())
        }
        ["anims", img, name] => show_anims(&Img::open(&root.join(img), &key)?, name),
        ["animscan"] => animscan(&root, &key),
        // root motion of clips (mover tracks + the root bone's first rotation key), per frame
        ["mover", img, name, ref clips @ ..] => show_mover(&Img::open(&root.join(img), &key)?, name, clips),
        // every car link as a CSV row, for matching against captured traffic (re/lanes.py):
        // from node (area, id, x, y, z, width, flags), to node (the same), link raw byte, length, flags
        // every path node (car and ped) as a TSV row: area, id, ped?, x, y, z, width, flags, and the
        // nodes it links to (research dump for the pedestrian crossings)
        ["pathnodes"] => {
            let img = Img::open(&root.join("pc/data/cdimages/paths.img"), &key)?;
            for i in 0..64u32 {
                let e = img.find(&format!("nodes{i}.nod")).context("nod not in archive")?;
                let a = rage::paths::read_nod(&img.read(e)?, i)?;
                for n in &a.nodes {
                    let links: Vec<String> = a.links[n.first_link as usize..(n.first_link + n.link_count as u32) as usize].iter().map(|l| format!("{}:{}", l.area, l.node)).collect();
                    println!("{}	{}	{}	{:.2}	{:.2}	{:.2}	{}	{:08x}	{}", n.area, n.id, n.is_ped as u8, n.pos[0], n.pos[1], n.pos[2], n.width, n.flags, links.join(" "));
                }
            }
            Ok(())
        }
        // stdin: "x y z" GTA positions (one per line, extra columns kept); stdout: each line plus what
        // the navmesh has there (pavement / other / nothing) and the distance to the nearest road
        // crossing (paths.img ped node pair). For scoring ped paths, real (session tracks) or ours.
        ["navclass"] => {
            use std::io::BufRead;
            let paths = Img::open(&root.join("pc/data/cdimages/paths.img"), &key)?;
            let mut nodes: Vec<[f32; 3]> = Vec::new();
            let mut pairs: Vec<(usize, usize)> = Vec::new();
            let mut base = Vec::new();
            let mut areas = Vec::new();
            for i in 0..64u32 {
                let e = paths.find(&format!("nodes{i}.nod")).context("nod not in archive")?;
                let a = rage::paths::read_nod(&paths.read(e)?, i)?;
                base.push(nodes.len());
                nodes.extend(a.nodes.iter().skip(a.car_nodes as usize).map(|n| n.pos));
                areas.push(a);
            }
            for (ai, a) in areas.iter().enumerate() {
                for (ni, n) in a.nodes.iter().enumerate().skip(a.car_nodes as usize) {
                    for l in &a.links[n.first_link as usize..(n.first_link + n.link_count as u32) as usize] {
                        let t = &areas[l.area as usize];
                        if (l.node as u32) >= t.car_nodes && (l.node as usize) < t.nodes.len() {
                            pairs.push((base[ai] + ni - a.car_nodes as usize, base[l.area as usize] + l.node as usize - t.car_nodes as usize));
                        }
                    }
                }
            }
            let img = Img::open(&root.join("pc/data/cdimages/navmeshes.img"), &key)?;
            let mut tiles: HashMap<u16, Option<rage::navmesh::NavMesh>> = HashMap::new();
            for line in std::io::stdin().lock().lines() {
                let line = line?;
                let v: Vec<f32> = line.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
                if v.len() < 3 {
                    continue;
                }
                let (x, y, z) = (v[0], v[1], v[2]);
                let mut class = "nothing";
                if let Some(area) = rage::navmesh::area_at(x, y) {
                    let tile = tiles.entry(area).or_insert_with(|| {
                        img.find(&rage::navmesh::tile_name(area)).and_then(|e| img.read(e).ok()).and_then(|b| Resource::load(&b).ok()).and_then(|r| rage::navmesh::read(&r).ok())
                    });
                    if let Some(t) = tile {
                        for p in &t.polys {
                            let n = p.corners.len();
                            let inside = (0..n).fold(false, |inside, i| {
                                let (a, b) = (p.corners[i], p.corners[(i + 1) % n]);
                                if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] { !inside } else { inside }
                            });
                            if inside && (p.centre()[2] - z).abs() < 2.5 {
                                if p.pavement() {
                                    class = "pavement";
                                    break;
                                }
                                class = "other";
                            }
                        }
                    }
                }
                // nearest crossing segment, on the ground plane
                let mut best = f32::MAX;
                for &(i, j) in &pairs {
                    let (a, b) = (nodes[i], nodes[j]);
                    if (a[0] - x).abs() > 60.0 || (a[1] - y).abs() > 60.0 {
                        continue;
                    }
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let t = (((x - a[0]) * dx + (y - a[1]) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
                    best = best.min(((a[0] + dx * t - x).powi(2) + (a[1] + dy * t - y).powi(2)).sqrt());
                }
                println!("{line}\t{class}\t{best:.2}");
            }
            Ok(())
        }
        ["pathlinks", out] => {
            let img = Img::open(&root.join("pc/data/cdimages/paths.img"), &key)?;
            let mut areas = Vec::with_capacity(64);
            for i in 0..64u32 {
                let name = format!("nodes{i}.nod");
                let e = img.find(&name).with_context(|| format!("{name} not in archive"))?;
                areas.push(rage::paths::read_nod(&img.read(e)?, i)?);
            }
            let mut csv = String::from("a_area,a_id,ax,ay,az,a_width,a_flags,b_area,b_id,bx,by,bz,b_width,b_flags,length,lanes,flags
");
            let mut n = 0;
            for a in &areas {
                for from in a.nodes.iter().filter(|n| !n.is_ped) {
                    for l in &a.links[from.first_link as usize..(from.first_link + from.link_count as u32) as usize] {
                        let Some(to) = areas.get(l.area as usize).and_then(|t| t.nodes.get(l.node as usize)).filter(|t| !t.is_ped) else { continue };
                        csv += &format!(
                            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}
",
                            from.area, from.id, from.pos[0], from.pos[1], from.pos[2], from.width, from.flags, to.area, to.id, to.pos[0], to.pos[1], to.pos[2], to.width, to.flags, l.length, l.lanes, l.flags
                        );
                        n += 1;
                    }
                }
            }
            std::fs::write(out, csv)?;
            println!("wrote {n} car links to {out}");
            Ok(())
        }
        ["paths"] => show_paths(&root, &key, Path::new(r"out\paths.png")),
        ["paths", out] => show_paths(&root, &key, Path::new(out)),
        _ => bail!(
            "usage: liberty list <img> | scan | textures <img> <name.wtd> <outdir> | raw <img> <name> <outdir> | water | vehicles | peds | rpf <path> | anims <img> <name.wad> | animscan | paths [out.png]"
        ),
    }
}

fn show_rpf(rpf: &Rpf) -> Result<()> {
    println!("{}: {} files ({})", rpf.path.display(), rpf.entries.len(),
        if rpf.encrypted { "encrypted" } else { "plain" });
    for e in &rpf.entries {
        println!("  {:40} {:>9} KB  type {:#04x}  flags {:#010x}", e.name, e.size / 1024, e.kind, e.flags);
    }
    let (mut ok, mut failed) = (0usize, 0usize);
    for e in rpf.entries.iter().filter(|e| e.size >= 4) {
        let bytes = rpf.read(e)?;
        if !bytes.starts_with(b"RSC\x05") {
            continue;
        }
        match Resource::load(&bytes) {
            Ok(_) => ok += 1,
            Err(err) => {
                failed += 1;
                println!("  RESOURCE FAIL {}: {err:#}", e.name);
            }
        }
    }
    println!("{ok} resources OK, {failed} failed");
    Ok(())
}

fn show_anims(img: &Img, name: &str) -> Result<()> {
    let entry = img.find(name).with_context(|| format!("{name} not in archive"))?;
    let res = Resource::load(&img.read(entry)?)?;
    let anims = anim::read_dictionary(&res)?;
    println!("{name}: {} animations", anims.len());
    for a in &anims {
        println!(
            "  {:32} {:>4} frames  {:>6.2}s  {:>3} tracks  {} skipped",
            a.name, a.frames, a.duration, a.tracks.len(), a.skipped_tracks
        );
    }
    Ok(())
}

fn show_mover(img: &Img, name: &str, clips: &[&str]) -> Result<()> {
    let entry = img.find(name).with_context(|| format!("{name} not in archive"))?;
    let anims = anim::read_dictionary(&Resource::load(&img.read(entry)?)?)?;
    for a in anims.iter().filter(|a| clips.is_empty() || clips.contains(&a.name.as_str())) {
        println!("{} {:.3}s {} frames", a.name, a.duration, a.frames);
        for t in &a.tracks {
            if !matches!(t.kind, anim::TrackKind::MoverTranslation | anim::TrackKind::MoverRotation) && t.bone_id != 0 {
                continue;
            }
            println!("  {:?} bone {} ({} keys)", t.kind, t.bone_id, t.frames.len());
            let n = t.frames.len();
            for (i, f) in t.frames.iter().enumerate() {
                if n <= 12 || i % (n / 12).max(1) == 0 || i + 1 == n {
                    println!("    {i:3} {:?}", f);
                }
            }
        }
    }
    Ok(())
}

/// Decode every .wad in pc/anim/anim.img and report aggregate stats — this is the acceptance
/// test for the format: a wrong decoder makes bogus (huge or zero) quaternion norms.
fn animscan(root: &Path, key: &Key) -> Result<()> {
    let img = Img::open(&root.join("pc/anim/anim.img"), key)?;
    let wads: Vec<_> = img.entries.iter().filter(|e| e.ext() == "wad").collect();

    let (mut total_anims, mut total_tracks) = (0usize, 0usize);
    let mut skipped_by_reason: BTreeMap<String, usize> = BTreeMap::new();
    let mut combos: BTreeMap<(u8, u8), usize> = BTreeMap::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let (mut rot_frames, mut rot_bad) = (0usize, 0usize);

    for e in &wads {
        let decoded = img.read(e).and_then(|b| Ok(Resource::load(&b)?)).and_then(|r| anim::read_dictionary(&r));
        let anims = match decoded {
            Ok(a) => a,
            Err(err) => {
                failures.push((e.name.clone(), format!("{err:#}")));
                continue;
            }
        };
        for a in &anims {
            total_anims += 1;
            total_tracks += a.tracks.len();
            for c in &a.channels {
                *combos.entry((c.track_type, c.layout)).or_default() += 1;
                if let Some(reason) = c.skip_reason {
                    let label = match reason {
                        anim::SkipReason::UnknownTrackType => "unknown track type".to_string(),
                        anim::SkipReason::UnknownLayout => "unknown layout".to_string(),
                        anim::SkipReason::UnsupportedEncoding(enc) => format!("encoding {enc:#04x}"),
                    };
                    *skipped_by_reason.entry(label).or_default() += 1;
                }
            }
            for t in &a.tracks {
                if matches!(t.kind, anim::TrackKind::Rotation | anim::TrackKind::MoverRotation) {
                    for f in &t.frames {
                        let len = (f[0] * f[0] + f[1] * f[1] + f[2] * f[2] + f[3] * f[3]).sqrt();
                        rot_frames += 1;
                        if (len - 1.0).abs() > 0.01 {
                            rot_bad += 1;
                        }
                    }
                }
            }
        }
    }

    println!("{} .wad files scanned", wads.len());
    println!("{total_anims} animations, {total_tracks} tracks");
    println!("skipped tracks by reason:");
    for (label, n) in &skipped_by_reason {
        println!("  x{n:<5} {label}");
    }
    println!("track type/layout combos seen:");
    for ((t, l), n) in &combos {
        println!("  x{n:<5} type {t:#04x} layout {l:#04x}");
    }
    println!("rotation quaternion norms: {rot_bad} / {rot_frames} frames with ||q| - 1| > 0.01");
    println!("{} parse failures", failures.len());
    for (name, err) in failures.iter().take(5) {
        println!("  {name}: {err}");
    }
    Ok(())
}

/// Decode all 64 `nodesN.nod` areas of `paths.img`, run the sanity checks from the brief, and
/// render a top-down PNG of the road/pavement network.
fn show_paths(root: &Path, key: &Key, out: &Path) -> Result<()> {
    let img = Img::open(&root.join("pc/data/cdimages/paths.img"), key)?;
    let mut areas: Vec<rage::paths::PathArea> = Vec::with_capacity(64);
    for i in 0..64u32 {
        let name = format!("nodes{i}.nod");
        let entry = img.find(&name).with_context(|| format!("{name} not in archive"))?;
        areas.push(rage::paths::read_nod(&img.read(entry)?, i)?);
    }

    let nonempty = areas.iter().filter(|a| !a.nodes.is_empty()).count();
    let total_nodes: usize = areas.iter().map(|a| a.nodes.len()).sum();
    let total_car: u32 = areas.iter().map(|a| a.car_nodes).sum();
    let total_ped = total_nodes - total_car as usize;
    let total_links: usize = areas.iter().map(|a| a.links.len()).sum();
    println!(
        "paths.img: 64 areas ({nonempty} non-empty), {total_nodes} nodes ({total_car} car, {total_ped} ped), {total_links} links"
    );

    // Link stats for traffic: is every car link matched by a reverse link, and what's in the
    // link's raw byte / flags (lane counts?) — histograms over car->car links.
    {
        use std::collections::{BTreeMap, HashSet};
        let mut edges: HashSet<(u16, u16, u16, u16)> = HashSet::new();
        let mut car_links = Vec::new();
        for (ai, a) in areas.iter().enumerate() {
            for (ni, n) in a.nodes.iter().enumerate().filter(|(_, n)| !n.is_ped) {
                for l in &a.links[n.first_link as usize..(n.first_link + n.link_count as u32) as usize] {
                    let to_ped = areas.get(l.area as usize).and_then(|t| t.nodes.get(l.node as usize)).is_none_or(|t| t.is_ped);
                    if !to_ped {
                        edges.insert((ai as u16, ni as u16, l.area, l.node));
                        car_links.push((ai as u16, ni as u16, *l));
                    }
                }
            }
        }
        let recip = car_links.iter().filter(|(a, n, l)| edges.contains(&(l.area, l.node, *a, *n))).count();
        println!("car->car links: {} ({} have a reverse link)", car_links.len(), recip);
        let mut raw: BTreeMap<u8, usize> = BTreeMap::new();
        let mut fl: BTreeMap<u16, usize> = BTreeMap::new();
        for (_, _, l) in &car_links {
            *raw.entry(l.lanes).or_default() += 1;
            *fl.entry(l.flags).or_default() += 1;
        }
        let top = |m: &BTreeMap<u16, usize>| {
            let mut v: Vec<_> = m.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            v.into_iter().take(14).map(|(k, c)| format!("{k:#06x}:{c}")).collect::<Vec<_>>().join(" ")
        };
        println!("  raw byte: {}", raw.iter().map(|(k, c)| format!("{k}:{c}")).collect::<Vec<_>>().join(" "));
        println!("  flags top: {}", top(&fl));
        for bit in 0..16 {
            let n = car_links.iter().filter(|(_, _, l)| l.flags & (1 << bit) != 0).count();
            if n > 0 {
                print!(" b{bit}:{n}");
            }
        }
        println!();
        let w: BTreeMap<u8, usize> = areas.iter().flat_map(|a| a.nodes.iter().filter(|n| !n.is_ped)).fold(BTreeMap::new(), |mut m, n| {
            *m.entry(n.width).or_default() += 1;
            m
        });
        let flag_of: std::collections::HashMap<(u16, u16, u16, u16), u16> =
            car_links.iter().map(|(a, n, l)| ((*a, *n, l.area, l.node), l.flags)).collect();
        let mut pairs: BTreeMap<(u16, u16), usize> = BTreeMap::new();
        for (a, n, l) in &car_links {
            *pairs.entry((l.flags, flag_of[&(l.area, l.node, *a, *n)])).or_default() += 1;
        }
        let mut pv: Vec<_> = pairs.into_iter().collect();
        pv.sort_by(|a, b| b.1.cmp(&a.1));
        println!("  (flags, reverse flags) top: {}", pv.iter().take(12).map(|((f, r), c)| format!("{f:#06x}/{r:#06x}:{c}")).collect::<Vec<_>>().join(" "));
        println!("  car node width: {}", w.iter().map(|(k, c)| format!("{k}:{c}")).collect::<Vec<_>>().join(" "));
    }

    // (a) link-count sum (from node flags bits 12-15) vs header link count, per file.
    let mut bad_sum = 0usize;
    for (i, a) in areas.iter().enumerate() {
        let sum: u32 = a.nodes.iter().map(|n| n.link_count as u32).sum();
        if sum != a.header_links {
            bad_sum += 1;
            if bad_sum <= 5 {
                println!("  area {i}: sum of node link counts {sum} != header link count {}", a.header_links);
            }
        }
    }
    println!("(a) {bad_sum}/{nonempty} non-empty areas where link-count sum != header link count");

    // (b) every link target (area, node) must exist.
    let (mut refs, mut missing) = (0usize, 0usize);
    for a in &areas {
        for l in &a.links {
            refs += 1;
            let ok = areas.get(l.area as usize).is_some_and(|t| t.nodes.get(l.node as usize).is_some());
            if !ok {
                missing += 1;
            }
        }
    }
    println!("(b) {missing}/{refs} link targets missing (area/node out of range)");

    // (c) straight-line distance vs the link's length byte, per link that resolves.
    let dist = |p: [f32; 3], q: [f32; 3]| {
        let d = [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    };
    let mut ratios: Vec<f32> = Vec::new();
    // (d) car/ped link direction counts.
    let mut dir_counts = [[0u32; 2]; 2]; // [from][to], 0 = car, 1 = ped
    // (e) does the node's raw "link" field equal the computed first-link (running sum) index?
    let (mut link_field_checked, mut link_field_match) = (0usize, 0usize);
    for a in &areas {
        for n in &a.nodes {
            if n.link_count > 0 {
                link_field_checked += 1;
                if n.link_field as u32 == n.first_link {
                    link_field_match += 1;
                }
            }
            for li in n.first_link..n.first_link + n.link_count as u32 {
                let Some(l) = a.links.get(li as usize) else { continue };
                let Some(t) = areas.get(l.area as usize).and_then(|ta| ta.nodes.get(l.node as usize)) else {
                    continue;
                };
                if l.length > 0 {
                    ratios.push(dist(n.pos, t.pos) / l.length as f32);
                }
                dir_counts[n.is_ped as usize][t.is_ped as usize] += 1;
            }
        }
    }
    ratios.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let mean_ratio = ratios.iter().sum::<f32>() / ratios.len().max(1) as f32;
    let median_ratio = ratios.get(ratios.len() / 2).copied().unwrap_or(0.0);
    println!(
        "(c) distance/length ratio over {} resolvable links: mean {mean_ratio:.3}, median {median_ratio:.3}",
        ratios.len()
    );
    println!(
        "(d) link direction counts: car->car {}, car->ped {}, ped->car {}, ped->ped {}",
        dir_counts[0][0], dir_counts[0][1], dir_counts[1][0], dir_counts[1][1]
    );
    println!(
        "(e) node \"link\" field == computed first-link index for {link_field_match}/{link_field_checked} nodes with link_count > 0"
    );

    render_map(&areas, out)?;
    println!("wrote {}", out.display());
    Ok(())
}

/// 2048x2048 top-down PNG: x,y in [-3000, 3000] GTA units, y up = image up. Car links grey, ped
/// links green, highway-flagged nodes red, water/boat nodes blue.
fn render_map(areas: &[rage::paths::PathArea], out: &Path) -> Result<()> {
    const SIZE: u32 = 2048;
    const WORLD: f32 = 6000.0;
    let to_px = |x: f32, y: f32| -> (i32, i32) {
        let px = (x + WORLD / 2.0) / WORLD * SIZE as f32;
        let py = (WORLD / 2.0 - y) / WORLD * SIZE as f32;
        (px as i32, py as i32)
    };
    let mut buf = image::RgbImage::from_pixel(SIZE, SIZE, image::Rgb([0, 0, 0]));
    let line = |buf: &mut image::RgbImage, (x0, y0): (i32, i32), (x1, y1): (i32, i32), c: [u8; 3]| {
        let (dx, dy) = ((x1 - x0).abs(), (y1 - y0).abs());
        let steps = dx.max(dy).max(1);
        for s in 0..=steps {
            let x = x0 + (x1 - x0) * s / steps;
            let y = y0 + (y1 - y0) * s / steps;
            if x >= 0 && y >= 0 && (x as u32) < SIZE && (y as u32) < SIZE {
                buf.put_pixel(x as u32, y as u32, image::Rgb(c));
            }
        }
    };
    for a in areas {
        for n in &a.nodes {
            for li in n.first_link..n.first_link + n.link_count as u32 {
                let Some(l) = a.links.get(li as usize) else { continue };
                let Some(t) = areas.get(l.area as usize).and_then(|ta| ta.nodes.get(l.node as usize)) else {
                    continue;
                };
                let colour = if n.is_ped || t.is_ped { [0, 160, 0] } else { [110, 110, 110] };
                line(&mut buf, to_px(n.pos[0], n.pos[1]), to_px(t.pos[0], t.pos[1]), colour);
            }
        }
    }
    for a in areas {
        for n in &a.nodes {
            let colour = if n.water() || n.boat() {
                Some([60, 60, 255])
            } else if n.behaviour() == 6 {
                Some([255, 40, 40])
            } else {
                None
            };
            if let Some(c) = colour {
                let (x, y) = to_px(n.pos[0], n.pos[1]);
                for oy in -1..=1 {
                    for ox in -1..=1 {
                        let (px, py) = (x + ox, y + oy);
                        if px >= 0 && py >= 0 && (px as u32) < SIZE && (py as u32) < SIZE {
                            buf.put_pixel(px as u32, py as u32, image::Rgb(c));
                        }
                    }
                }
            }
        }
    }
    std::fs::create_dir_all(out.parent().unwrap_or(Path::new(".")))?;
    buf.save(out)?;
    Ok(())
}

fn list(img: &Img) -> Result<()> {
    let mut by_ext: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &img.entries {
        *by_ext.entry(e.ext()).or_default() += 1;
    }
    println!("{}: {} files ({})", img.path.display(), img.entries.len(),
        if img.encrypted { "encrypted" } else { "plain" });
    println!("  {by_ext:?}");
    for e in &img.entries {
        println!("  {:40} {:>9} KB  type {:#04x}", e.name, e.size / 1024, e.kind);
    }
    Ok(())
}

/// Every shader used by the .wdr models of an archive: how many materials, and how many of them
/// name a normal map / a specular map (with an example specular texture).
fn shader_stats(img: &Img) -> Result<()> {
    let mut stats: BTreeMap<String, (usize, usize, usize, String)> = BTreeMap::new();
    let mut no_diffuse: BTreeMap<String, usize> = BTreeMap::new();
    for e in img.entries.iter().filter(|e| e.ext() == "wdr") {
        let Ok(d) = Resource::load(&img.read(e)?).and_then(|r| drawable::read(&r)) else { continue };
        for s in &d.shaders {
            let st = stats.entry(s.name.clone()).or_default();
            st.0 += 1;
            if s.diffuse.is_none() {
                *no_diffuse.entry(s.name.clone()).or_default() += 1;
            }
            st.1 += s.normal.is_some() as usize;
            st.2 += s.specular.is_some() as usize;
            if st.3.is_empty() {
                st.3 = s.specular.clone().unwrap_or_default();
            }
        }
    }
    println!("{:44} {:>6} {:>7} {:>8}  example specular texture", "shader", "used", "normal", "specular");
    for (name, (n, nm, sp, ex)) in &stats {
        println!("{name:44} {n:>6} {nm:>7} {sp:>8}  {ex}");
    }
    println!("materials with no diffuse texture, by shader: {no_diffuse:?}");
    Ok(())
}

/// Open every .img under the game folder and inflate every resource in it.
fn scan(root: &Path, key: &Key) -> Result<()> {
    let mut imgs = Vec::new();
    walk(root, &mut imgs)?;
    let (mut ok, mut failed, mut plain) = (0usize, 0usize, 0usize);
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    let (mut textures, mut models, mut triangles) = (0usize, 0usize, 0usize);
    let mut col = rage::bounds::CollisionMesh::default();
    let mut wbn_files = 0usize;
    let mut triangles_col = 0usize;
    for path in &imgs {
        let img = match Img::open(path, key) {
            Ok(i) => i,
            Err(e) => {
                println!("ARCHIVE FAIL {}: {e:#}", path.display());
                continue;
            }
        };
        for e in &img.entries {
            let bytes = img.read(e)?;
            if !bytes.starts_with(b"RSC\x05") {
                plain += 1;
                continue;
            }
            let res = Resource::load(&bytes).and_then(|r| {
                match e.ext() {
                    "wtd" => textures += wtd::read(&r)?.len(),
                    "wdr" => {
                        let d = drawable::read(&r)?;
                        models += 1;
                        triangles += d.meshes.iter().map(|m| m.indices.len() / 3).sum::<usize>();
                    }
                    "wdd" => models += drawable::read_dictionary(&r)?.len(),
                    "wbn" => {
                        let m = rage::bounds::read_wbn(&r)?;
                        wbn_files += 1;
                        col.polygons += m.polygons;
                        col.unmatched += m.unmatched;
                        col.skipped += m.skipped;
                        triangles_col += m.triangles.len();
                    }
                    _ => {}
                }
                Ok(r)
            });
            match res {
                Ok(_) => ok += 1,
                Err(err) => {
                    failed += 1;
                    let msg = format!("{}: {err:#}", e.ext());
                    let n = failures.entry(msg.clone()).or_default();
                    if *n == 0 {
                        println!("  first fail: {} / {} -> {err:#}", path.display(), e.name);
                    }
                    *n += 1;
                }
            }
        }
    }
    println!("\n{} archives, {ok} resources OK, {failed} failed, {plain} non-resource files", imgs.len());
    println!("{textures} textures parsed from .wtd files");
    println!("{wbn_files} .wbn collision files: {} polygons -> {triangles_col} triangles, {} area mismatches, {} unread bound types", col.polygons, col.unmatched, col.skipped);
    println!("{models} models parsed from .wdr/.wdd files ({triangles} triangles in the .wdr ones)");
    for (msg, n) in failures {
        println!("  x{n:<5} {msg}");
    }
    Ok(())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("img")) {
            out.push(p);
        }
    }
    Ok(())
}

/// Which placed models of a chunk have no .wdr anywhere, and what we know about them.
fn missing_models(root: &Path, key: &Key, chunk: &str) -> Result<()> {
    let index = rage::index::GameIndex::build(root, key)?;
    let a = index.archive(&format!("/{chunk}.img")).context("no such chunk")?;
    let img = &index.imgs[a];
    let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
    for e in img.entries.iter().filter(|e| e.ext() == "wpl") {
        for i in rage::placement::read_wpl(&img.read(e)?)? {
            if !index.models.contains_key(&i.model_hash) {
                *counts.entry(i.model_hash).or_default() += 1;
            }
        }
    }
    let (named, unnamed): (Vec<_>, Vec<_>) = counts.iter().partition(|(h, _)| index.model_name.contains_key(h));
    println!("{} missing models ({} placements); {} have IDE names, {} unknown hashes",
        counts.len(), counts.values().sum::<usize>(), named.len(), unnamed.len());
    let mut named = named;
    named.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (h, n) in named.iter().take(25) {
        println!("  x{n:<4} {:08x} {}  txd={:?}", h, index.model_name[h], index.model_txd.get(h));
    }
    for (h, n) in unnamed.iter().take(5) {
        println!("  x{n:<4} {h:08x} (no IDE entry)");
    }
    Ok(())
}

/// Everything the map places within `r` metres of a GTA position: name, where it stands, which
/// file holds the model, and the lights that file carries.
fn near(root: &Path, key: &Key, x: f32, y: f32, r: f32) -> Result<()> {
    let index = rage::index::GameIndex::build(root, key)?;
    let mut rows: Vec<(f32, String)> = Vec::new();
    for img in &index.imgs {
        let loose = img.path.with_extension("wpl");
        if !loose.exists() {
            continue;
        }
        let chunk = img.path.file_stem().unwrap().to_string_lossy().to_string();
        let mut all = rage::placement::read_wpl(&std::fs::read(&loose)?)?;
        for e in img.entries.iter().filter(|e| e.ext() == "wpl") {
            all.extend(rage::placement::read_wpl(&img.read(e)?)?);
        }
        for i in all {
            let d = ((i.position[0] - x).powi(2) + (i.position[1] - y).powi(2)).sqrt();
            if d > r {
                continue;
            }
            let name = index.model_name.get(&i.model_hash).cloned().unwrap_or_else(|| format!("{:08x}", i.model_hash));
            let file = match index.models.get(&i.model_hash) {
                Some(&(a, e)) => {
                    let m = &index.imgs[a];
                    let ent = &m.entries[e];
                    let lights = Resource::load(&m.read(ent)?).ok().and_then(|res| if ent.ext() == "wft" { drawable::read_fragment(&res).ok() } else { drawable::read(&res).ok() }).map(|d| d.lights);
                    let l = match lights {
                        Some(l) if !l.is_empty() => format!("{} lights: {}", l.len(), l.iter().take(4).map(|l| format!("[{:02x}{:02x}{:02x} fill {} range {:.0} strength {:.0} kind {} cone {:.0} at {:.1} {:.1} {:.1} dir {:.2} {:.2} {:.2}]", l.color[0], l.color[1], l.color[2], format!("{:?} flags {:#x}", l.extra, l.flags), l.range, l.intensity, l.kind, l.cone, l.pos[0], l.pos[1], l.pos[2], l.dir[0], l.dir[1], l.dir[2])).collect::<Vec<_>>().join(" ")),
                        Some(_) => "no lights".into(),
                        None => "UNREADABLE".into(),
                    };
                    format!("{}.{} in {}: {l}", name, ent.ext(), m.path.file_name().unwrap().to_string_lossy())
                }
                None => format!("{name}: no .wdr / .wft of its own (dictionary or missing)"),
            };
            rows.push((d, format!("{d:5.1} m  at {:7.1} {:7.1} {:5.1}  lod {:4}  [{chunk}] {file}", i.position[0], i.position[1], i.position[2], i.lod)));
        }
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, l) in &rows {
        println!("{l}");
    }
    println!("{} placements within {r} m of ({x}, {y})", rows.len());
    Ok(())
}

fn placed(root: &Path, key: &Key, text: &str) -> Result<()> {
    let index = rage::index::GameIndex::build(root, key)?;
    let text = text.to_ascii_lowercase();
    for img in &index.imgs {
        let loose = img.path.with_extension("wpl");
        if !loose.exists() {
            continue;
        }
        let chunk = img.path.file_stem().unwrap().to_string_lossy().to_string();
        let mut all = rage::placement::read_wpl(&std::fs::read(&loose)?)?;
        for e in img.entries.iter().filter(|e| e.ext() == "wpl") {
            all.extend(rage::placement::read_wpl(&img.read(e)?)?);
        }
        for i in all {
            let name = index.model_name.get(&i.model_hash).cloned().unwrap_or_default();
            if name.to_ascii_lowercase().contains(&text) {
                println!("{chunk}	{name}	{:.1}	{:.1}	{:.1}	{:.3} {:.3} {:.3} {:.3}", i.position[0], i.position[1], i.position[2], i.rotation[0], i.rotation[1], i.rotation[2], i.rotation[3]);
            }
        }
    }
    Ok(())
}

fn fragsweep(root: &Path, key: &Key) -> Result<()> {
    let index = rage::index::GameIndex::build(root, key)?;
    let (mut total, mut no_set, mut set_turned, mut turned_bones, mut moved) = (0, 0, 0, 0, 0);
    for (hash, &(a, e)) in &index.models {
        let m = &index.imgs[a];
        let ent = &m.entries[e];
        if ent.ext() != "wft" {
            continue;
        }
        let Ok(res) = Resource::load(&m.read(ent)?) else { continue };
        let name = index.model_name.get(hash).cloned().unwrap_or_default();
        let r = Resource::ROOT;
        let Ok(d) = res.u32(r, 0xB4) else { continue };
        if d == 0 {
            continue;
        }
        total += 1;
        let bones = drawable::read_skeleton(&res, d).unwrap_or_default();
        let set = res.u32(r, 0x1E4).unwrap_or(0);
        if set == 0 || res.u32(set, 0).unwrap_or(0) as usize != bones.len() {
            no_set += 1;
        }
        let mats = drawable::rest_matrices(&res, &bones);
        if set != 0 && mats.iter().any(|m| (m[0][0] - 1.0).abs() > 1e-3 || (m[1][1] - 1.0).abs() > 1e-3 || (m[2][2] - 1.0).abs() > 1e-3) {
            set_turned += 1;
            println!("stored turn: {name}");
        }
        if bones.iter().any(|b| b.local_rot[3].abs() < 0.9999) {
            turned_bones += 1;
            println!("turned bone: {name} ({})", bones.iter().filter(|b| b.local_rot[3].abs() < 0.9999).map(|b| b.name.as_str()).collect::<Vec<_>>().join(" "));
        }
        let (list, count) = (res.u32(r, 0xD4).unwrap_or(0), res.u8(r, 0x1F3).unwrap_or(0) as usize);
        let mut bad = Vec::new();
        for i in 0..count {
            let Ok(child) = res.u32(list, i * 4) else { continue };
            let cd = res.u32(child, 0x90).unwrap_or(0);
            if cd == 0 {
                continue;
            }
            let c = res.u16(child, 0x0C).unwrap_or(0);
            let lod = res.u32(cd, 0x40).unwrap_or(0);
            if lod == 0 {
                continue;
            }
            let models = res.u32(lod, 0).unwrap_or(0);
            for k in 0..res.u16(lod, 4).unwrap_or(0) as usize {
                let model = res.u32(models, k * 4).unwrap_or(0);
                let mi = res.u8(model, 0x17).unwrap_or(0) as u16;
                if mi != c {
                    bad.push(format!("child {i} +0C {c} model {mi}"));
                }
            }
        }
        if !bad.is_empty() {
            moved += 1;
            println!("moved: {name}: {}", bad.join(", "));
        }
    }
    println!("{total} fragments; {no_set} without a matching stored set; {set_turned} with a turned stored matrix; {turned_bones} with turned bones; {moved} whose child +0x0C differs from the model's matrix index");
    Ok(())
}

fn show_wpl(bytes: &[u8], name: &str) -> Result<()> {
    let counts: Vec<u32> = (0..17).map(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())).collect();
    println!("{name}: {} bytes, version {}, section counts {:?}", bytes.len(), counts[0], &counts[1..]);
    let inst = rage::placement::read_wpl(bytes)?;
    for i in inst.iter().take(5) {
        println!("  {:08x} at {:?} rot {:?} flags {:#x} lod {}", i.model_hash, i.position, i.rotation, i.flags, i.lod);
    }
    Ok(())
}

/// Summarize `common/data/water.dat`: quad/degenerate counts, XY bounds, a Z histogram and
/// per-kind counts.
fn show_water(root: &Path) -> Result<()> {
    let text = std::fs::read_to_string(root.join("common/data/water.dat"))?;
    let total_lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    let quads = rage::water::read_water(&text)?;
    let skipped = total_lines - quads.len();

    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut z_hist: BTreeMap<i32, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<u32, usize> = BTreeMap::new();
    for q in &quads {
        for c in &q.corners {
            min_x = min_x.min(c[0]);
            max_x = max_x.max(c[0]);
            min_y = min_y.min(c[1]);
            max_y = max_y.max(c[1]);
            *z_hist.entry((c[2] * 2.0).round() as i32).or_default() += 1;
        }
        *kinds.entry(q.kind).or_default() += 1;
    }

    println!("water.dat: {} quads ({skipped} degenerate skipped)", quads.len());
    println!("  XY bounds: ({min_x}, {min_y}) .. ({max_x}, {max_y})");
    let mut by_count: Vec<(i32, usize)> = z_hist.into_iter().collect();
    by_count.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("  Z histogram (top 10, 0.5m buckets):");
    for (bucket, n) in by_count.iter().take(10) {
        println!("    {:>7.1} m  x{n}", *bucket as f32 / 2.0);
    }
    println!("  kinds:");
    for (kind, n) in &kinds {
        println!("    kind {kind:>3}  x{n}");
    }
    Ok(())
}

/// Summarize `common/data/handling.dat`, `vehicles.ide` and `carcols.dat`: entry counts, then a
/// table of a few well-known vehicles.
fn show_vehicles(root: &Path) -> Result<()> {
    let handling_text = std::fs::read_to_string(root.join("common/data/handling.dat"))?;
    let data_lines = handling_text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with(';'))
        .count();
    let handling = rage::vehicles::read_handling(&handling_text)?;
    let skipped = data_lines - handling.len();

    let ide_text = std::fs::read_to_string(root.join("common/data/vehicles.ide"))?;
    let defs = rage::vehicles::read_vehicles_ide(&ide_text)?;

    let carcols_text = std::fs::read_to_string(root.join("common/data/carcols.dat"))?;
    let colours = rage::vehicles::read_carcols(&carcols_text)?;
    let cars_with_combos = colours.combos.values().filter(|c| !c.is_empty()).count();

    println!(
        "handling.dat: {} entries ({skipped} lines skipped)",
        handling.len()
    );
    println!("vehicles.ide: {} vehicle defs", defs.len());
    println!(
        "carcols.dat: {} palette colours, {cars_with_combos} cars with colour combos",
        colours.palette.len()
    );
    println!(
        "\n{:10} {:>8} {:>6} {:>6} {:>6} {:>6}  traction max/min  first combo",
        "id", "mass", "force", "km/h", "brake", "steer"
    );
    for name in ["admiral", "banshee", "infernus", "taxi", "bus"] {
        let Some(h) = handling.get(name) else {
            println!("  {name}: no handling entry");
            continue;
        };
        let combo = colours
            .combos
            .get(name)
            .and_then(|c| c.first())
            .map(|idx| {
                idx.iter()
                    .filter_map(|i| colours.palette.get(*i as usize))
                    .map(|[r, g, b]| format!("({r},{g},{b})"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:10} {:>8.1} {:>6.2} {:>6.1} {:>6.2} {:>6.1}  {:.2}/{:.2}  {combo}",
            h.id,
            h.mass,
            h.drive_force,
            h.max_velocity_kmh,
            h.brake_force,
            h.steering_lock_deg,
            h.traction_curve_max,
            h.traction_curve_min,
        );
    }
    Ok(())
}

/// Check `pedVariations.dat`/`peds.ide`/`moveblend.dat` against `anim.img` (movement groups) and
/// `componentpeds.img` (component drawable dictionaries).
fn show_peds(root: &Path, key: &Key) -> Result<()> {
    let variations_text = std::fs::read_to_string(root.join("common/data/pedVariations.dat"))?;
    let variations = rage::peds::read_ped_variations(&variations_text)?;

    let ide_text = std::fs::read_to_string(root.join("common/data/peds.ide"))?;
    let defs = rage::peds::read_peds_ide(&ide_text)?;

    let moveblend_text = std::fs::read_to_string(root.join("common/data/moveblend.dat"))?;
    let moveblend = rage::peds::read_moveblend(&moveblend_text)?;

    let with_variation = defs.iter().filter(|d| variations.contains_key(&d.model)).count();

    let mut move_groups: BTreeMap<&str, usize> = BTreeMap::new();
    for d in &defs {
        *move_groups.entry(d.move_group.as_str()).or_default() += 1;
    }
    let anim_img = Img::open(&root.join("pc/anim/anim.img"), key)?;
    let groups_with_wad = move_groups.keys().filter(|g| anim_img.find(&format!("{g}.wad")).is_some()).count();

    println!("pedVariations.dat: {} peds", variations.len());
    println!("peds.ide: {} peds ({with_variation} have a pedVariations.dat entry)", defs.len());
    println!("moveblend.dat: {} movement groups (fallbacks)", moveblend.len());
    println!(
        "{} distinct move groups used in peds.ide, {groups_with_wad} have a .wad in pc/anim/anim.img",
        move_groups.len()
    );

    let component_img = Img::open(&root.join("pc/models/cdimages/componentpeds.img"), key)?;
    let (mut checked, mut resolved, mut missing) = (0usize, 0usize, 0usize);
    let mut misses: Vec<(String, String, u32)> = Vec::new();
    let mut unreferenced = 0usize;
    for d in &defs {
        let Some(entry) = component_img.find(&format!("{}.wdd", d.model)) else { continue };
        let Some(variation) = variations.get(&d.model) else { continue };
        let res = Resource::load(&component_img.read(entry)?)?;
        let dict = drawable::read_dictionary(&res)?;
        checked += 1;
        let mut used = vec![false; dict.len()];
        for slot in variation.slots.iter().filter(|s| !s.is_prop()) {
            let hashes = rage::peds::drawable_names(&slot.slot, slot.geometry).map(|n| rage::placement::hash(&n));
            let hit = dict.iter().position(|(h, _)| hashes.contains(h));
            match hit {
                Some(i) => {
                    used[i] = true;
                    resolved += 1;
                }
                None => {
                    missing += 1;
                    if misses.len() < 10 {
                        misses.push((d.model.clone(), slot.slot.clone(), slot.geometry));
                    }
                }
            }
        }
        unreferenced += used.iter().filter(|u| !**u).count();
    }
    println!(
        "\ncomponentpeds.img: {checked} peds checked, {resolved} slot variants resolved, {missing} missing"
    );
    for (model, slot, geometry) in &misses {
        println!("  miss: {model} {slot} geometry {geometry}");
    }
    println!("{unreferenced} dictionary drawables not referenced by any variant");

    if let (Some(entry), Some(variation)) =
        (component_img.find("m_y_pcool_01.wdd"), variations.get("m_y_pcool_01"))
    {
        let res = Resource::load(&component_img.read(entry)?)?;
        let dict = drawable::read_dictionary(&res)?;
        println!("\nm_y_pcool_01 slots:");
        for slot in variation.slots.iter().filter(|s| !s.is_prop()) {
            let names = rage::peds::drawable_names(&slot.slot, slot.geometry);
            let matched = names
                .iter()
                .find(|n| dict.iter().any(|(h, _)| *h == rage::placement::hash(n)))
                .map_or("none".to_string(), |n| n.rsplit('_').next().unwrap().to_string());
            println!("  {} geometry {}: {matched}", slot.slot, slot.geometry);
        }
    }
    Ok(())
}

fn show_model(img: &Img, name: &str) -> Result<()> {
    let entry = img.find(name).with_context(|| format!("{name} not in archive"))?;
    let res = Resource::load(&img.read(entry)?)?;
    let d = if entry.ext() == "wft" { drawable::read_fragment(&res)? } else { drawable::read(&res)? };
    println!("{name}: bounds {:?} .. {:?}", d.bounds_min, d.bounds_max);
    // a plain drawable's own skeleton (weapons: gun_grip, gun_grip2, gun_muzzle, gun_ejac)
    if entry.ext() == "wdr" {
        for (i, b) in drawable::read_skeleton(&res, Resource::ROOT)?.iter().enumerate() {
            println!("  bone {i}: {:<12} id {:5} parent {:?} local_pos {:?} local_rot {:?} abs_pos {:?}", b.name, b.id, b.parent, b.local_pos, b.local_rot, b.abs_pos);
        }
    }
    for (i, s) in d.shaders.iter().enumerate() {
        println!("  shader {i}: {} diffuse={:?} normal={:?} specular={:?} other textures={:x?}", s.name, s.diffuse, s.normal, s.specular, s.layers);
    }
    for m in &d.meshes {
        if !m.colors.is_empty() {
            let mean = |c: usize| m.colors.iter().map(|x| x[c] as f32).sum::<f32>() / m.colors.len() as f32;
            let lo = |c: usize| m.colors.iter().map(|x| x[c]).min().unwrap();
            let hi = |c: usize| m.colors.iter().map(|x| x[c]).max().unwrap();
            println!("  vertex colours ({}): red mean {:.0} ({}..{}), green mean {:.0} ({}..{}), blue mean {:.0}, alpha mean {:.0}", m.colors.len(), mean(0), lo(0), hi(0), mean(1), lo(1), hi(1), mean(2), mean(3));
        }
        println!("  mesh: {} verts, {} tris, shader {}, normals {}, uvs {}, tangents {} (first {:?}), first pos {:?}",
            m.positions.len(), m.indices.len() / 3, m.shader, m.normals.len(), m.uvs.len(), m.tangents.len(), m.tangents.first(),
            m.positions.first());
        // normal-mapped geometry: do the stored tangents follow +u, and does cross(N, T) * w (the
        // bitangent a renderer builds) follow +v? (fraction of triangle corners that agree)
        if m.tangents.len() == m.positions.len() && m.uvs.len() == m.positions.len() && m.normals.len() == m.positions.len() {
            let v3 = |a: [f32; 3]| a.map(|x| x as f64);
            let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
            let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
            let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let (mut t_ok, mut b_ok, mut n) = (0u32, 0u32, 0u32);
            for tri in m.indices.chunks_exact(3) {
                let [i0, i1, i2] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
                let (e1, e2) = (sub(v3(m.positions[i1]), v3(m.positions[i0])), sub(v3(m.positions[i2]), v3(m.positions[i0])));
                let (du1, dv1) = ((m.uvs[i1][0] - m.uvs[i0][0]) as f64, (m.uvs[i1][1] - m.uvs[i0][1]) as f64);
                let (du2, dv2) = ((m.uvs[i2][0] - m.uvs[i0][0]) as f64, (m.uvs[i2][1] - m.uvs[i0][1]) as f64);
                let det = du1 * dv2 - du2 * dv1;
                if det.abs() < 1e-12 {
                    continue;
                }
                let dpdu = [0, 1, 2].map(|k| (e1[k] * dv2 - e2[k] * dv1) / det);
                let dpdv = [0, 1, 2].map(|k| (e2[k] * du1 - e1[k] * du2) / det);
                for i in [i0, i1, i2] {
                    let t = m.tangents[i];
                    let tv = [t[0] as f64, t[1] as f64, t[2] as f64];
                    let b = cross(v3(m.normals[i]), tv).map(|x| x * t[3] as f64);
                    n += 1;
                    t_ok += (dot(tv, dpdu) > 0.0) as u32;
                    b_ok += (dot(b, dpdv) > 0.0) as u32;
                }
            }
            if n > 0 {
                println!("        tangent follows +u on {:.0}% of corners; cross(N,T)*w follows +v on {:.0}%", 100.0 * t_ok as f64 / n as f64, 100.0 * b_ok as f64 / n as f64);
            }
        }
    }
    println!("  embedded textures: {}", d.textures.len());
    Ok(())
}

/// Write a resource's inflated segments to `<dir>/<name>.sys` / `.gfx` for hex poking.
fn dump_raw(img: &Img, name: &str, dir: &Path) -> Result<()> {
    let entry = img.find(name).with_context(|| format!("{name} not in archive"))?;
    let res = Resource::load(&img.read(entry)?)?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("{name}.sys")), &res.system)?;
    std::fs::write(dir.join(format!("{name}.gfx")), &res.graphics)?;
    println!("{name}: type {:#x}, system {} B, graphics {} B", res.kind, res.system.len(), res.graphics.len());
    Ok(())
}

fn dump_textures(img: &Img, name: &str, dir: &Path) -> Result<()> {
    let entry = img.find(name).with_context(|| format!("{name} not in archive"))?;
    let res = Resource::load(&img.read(entry)?)?;
    std::fs::create_dir_all(dir)?;
    for t in wtd::read(&res)? {
        let rgba = decode(&t)?;
        let file = dir.join(format!("{}.png", t.name));
        image::save_buffer(&file, &rgba, t.width as u32, t.height as u32, image::ColorType::Rgba8)?;
        println!("  {:32} {:>4}x{:<4} {:?} ({} mips)", t.name, t.width, t.height, t.format, t.levels);
    }
    Ok(())
}

/// Decode the top mip level to RGBA8.
fn decode(t: &Texture) -> Result<Vec<u8>> {
    let (w, h) = (t.width as usize, t.height as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    match t.format {
        Format::Argb8 => {
            for px in t.level0().chunks_exact(4) {
                rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        Format::L8 => {
            for &l in t.level0() {
                rgba.extend_from_slice(&[l, l, l, 255]);
            }
        }
        dxt => {
            let mut px = vec![0u32; w * h];
            let r = match dxt {
                Format::Dxt1 => texture2ddecoder::decode_bc1(t.level0(), w, h, &mut px),
                Format::Dxt3 => texture2ddecoder::decode_bc2(t.level0(), w, h, &mut px),
                _ => texture2ddecoder::decode_bc3(t.level0(), w, h, &mut px),
            };
            r.map_err(|e| anyhow::anyhow!("{}: {e}", t.name))?;
            // decoder packs pixels as 0xAARRGGBB
            for p in px {
                rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
            }
        }
    }
    Ok(rgba)
}

fn show_weapons(root: &Path) -> Result<()> {
    let ws = rage::weapons::load(root)?;
    println!("{:16} {:9} {:12} {:>4} {:>5} {:>5} {:>5} {:>6} {:>6}  {:14} {}", "weapon", "slot", "fire", "dmg", "range", "clip", "rps", "reload", "force", "model", "anim group");
    for w in &ws {
        println!("{:16} {:9} {:12} {:4} {:5.1} {:5} {:5.1} {:6.0} {:6.1}  {:14} {}", w.name, w.slot, w.fire_type, w.damage, w.weapon_range, w.clip_size, w.rate_of_fire(), w.reload_ms, w.force, w.model, w.anim_group);
    }
    println!("{} weapons", ws.len());
    Ok(())
}
