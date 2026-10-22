# Liberty — GTA IV engine rewrite (working name)

Goal: load the user's own GTA IV install into our own engine (Rust + Bevy, iw4L-style), then
walk/drive around Liberty City with our own mechanics. Long-term: same pipeline for GTA V.
Private project — never ship game files, never print or save the AES key.

Install: `C:\Program Files (x86)\Steam\steamapps\common\Grand Theft Auto IV\GTAIV`
(exe 1.2.0.59, Complete Edition: base + TLAD + TBoGT, ~19 GB of .img/.rpf)

## Verified (2026-09-26, probe/img_probe.py)
- AES key read from GTAIV.exe at 0xC5B73C, SHA1 fingerprint matches gtamods. Scan fallback exists.
- Crypto: AES-256-ECB, decrypt each 16-byte block 16 times; trailing partial block is plain.
- IMG v3: **only header bytes 0-15 are encrypted**; bytes 16-19 plain (item size 16, unknown).
  Table at offset 20 = its own encrypted block run, `count*16` entries then NUL-separated names.
  Entry = u32 RSC flags (not a byte size) · u32 type · u32 sector · u16 blocks · u16 extra.
  Extent on disk = blocks*2048 at sector*2048.
- Every resource inside starts `RSC\x05` (RSC5 container). Types seen: wdr/wdd (models),
  wft (vehicles/fragments), wtd (textures), wbn (collision), wpl (placements).

- RSC5: 12-byte header (`RSC\x05`, type, flags), zlib body → system segment then graphics segment.
  Sizes: sys = (f & 0x7FF) << (((f>>11)&0xF)+8), gfx = ((f>>15)&0x7FF) << (((f>>26)&0xF)+8).
  Pointers 0x5xxxxxxx = system offset, 0x6xxxxxxx = graphics offset. Root object at 0x50000000.
- WTD (`pgDictionary<grcTexturePC>`): root +0x10 name hashes, +0x18 texture ptr list, +0x1C count.
  Texture: +0x14 name ptr (`pack:/x.dds`, may be 0 → use hash), +0x1C/+0x1E w/h, +0x20 D3D format
  (DXT1/3/5 FourCC, 21 = A8R8G8B8, 50 = L8), +0x27 mip count, pixels at +0x48 (type 8) or +0x44
  (older type 7, e.g. slod_*).

## Code (Rust workspace, build output in target)
- `crates/rage` — crypto, img, rsc, wtd readers.
- `crates/liberty-cli` → `liberty list|scan|textures|raw` for checking readers against the game.
- `liberty scan` = regression test: 254 archives, 46,654 resources, 70,706 textures, 0 failures, ~75 s.

- WDR/WDD (`crates/rage/src/drawable.rs`, offsets in its header comment): drawable → shader group
  (diffuse = texture param with name hash 0x2b5170fd) → models (LOD 0 at +0x40) → geometries →
  vertex/index buffers. Vertex decl: 16 usage bits (0 pos, 3 normal, 6.. uv) with a 4-bit type each;
  offsets = running sum of present elements, must equal stride. Only triangle lists (prim type 3).
  `liberty scan`: 40,319 models, 41.1M triangles in .wdr files, 0 failures.
- Placement (`placement.rs`): model hash = Jenkins one-at-a-time on lowercase name; WPL = u32
  version 3 + 16 section counts (68 B), INST entries 48 B (pos, quat, hash, flags, lod).
  WPL quaternions are stored inverted → conjugate. IDE `objs`/`tobj`/`anim` lines: name, txd,
  draw distance; `txdp` lines: child pack, parent pack.
- A chunk's placements = every `.wpl` inside `<chunk>.img` (`_strbig*` big buildings, `_stream*`
  props/detail) **plus** the loose `<chunk>.wpl` beside it, minus entries that something's `lod`
  field points at (those are the low-detail stand-ins; the rest is real terrain, trees, fences).
- Shared parent texture packs (mh_roads, mh_buildings, ...) live in `pc/data/cdimages/gtxd.img`.
- WFT fragments (breakable props, vehicles): drawable at root +0xB4 holds shaders; props keep the
  geometry in children (list +0xD4, count byte +0x1F3, child drawable at +0x90), already in model
  space. Some drawables have no LOD 0 (use first present LOD) or no geometry at all (lights).
- `rage::index::GameIndex` indexes all base-game map archives + gtxd.img + IDEs in ~0.2 s.

## Viewer (`crates/liberty-view`, Bevy 0.16 pinned)
- `View-Building.bat` or `liberty-view [img] [model.wdr]`; `--shot x.png` renders, saves, quits;
  `LIBERTY_CAM_YAW=deg` orbits the start camera. GTA Z-up → Bevy (x, z, -y).
- DXT textures go to the GPU compressed with all mips (BC1/2/3 sRGB); L8 expanded to RGBA.
- Verified: 253bway_mh2.wdr renders correctly from all sides (out/shots/).
- Known gap: shared textures like `ws_mh_commonroof*_big` live in parent txds (gtxd.ide).

- Chunk mode (`View-Chunk.bat` / `--chunk manhat02[,manhat03...]`): manhat02 = 5,767 objects,
  549 models, 3.8M triangles, 1,664 textures, loads in ~2.5 s; 2 placements have no model anywhere.
  Five chunks = 27k objects / 17M triangles in ~27 s (debug build, no LOD/streaming yet).
  `LIBERTY_CAM_AT=<model> LIBERTY_CAM_YAW=deg LIBERTY_CAM_OFF=dist,height,target_h` for shots.

## Water (`rage::water`, written by a worker subagent)
- `common/data/water.dat`: text, one quad per line = 4 × (x y z + 4 params) + int kind + float;
  corners v0=(x?,y1) v1=(x?,y1) v2/v3 at y2 → triangles 0-1-2, 2-1-3. 399 quads (+4 degenerate),
  ±3 km; kinds 5 (sea, most), 9, 13 (raised ponds/pools, 8–28 m), 8.
- Viewer adds a ring of open sea from ±3 km to ±40 km (not a plane under everything — would cut
  the subway). No reflections yet, so it's a semi-rough tinted surface.
- Debug lesson: grey water from above isn't missing water — check with an unlit red material.

## Streaming + LOD (`crates/liberty-view/src/{world,stream}.rs`)
- `--city city` (or `manhat*`, comma lists) plans all 34 chunks of the three islands (a chunk =
  map archive with a loose `.wpl` beside it). Planning is cheap (WPL + IDE only).
- Stand-ins (`lod_*`, in each chunk's .wdd dictionaries) for the whole city load first as one
  background job; then HD chunks load one at a time, nearest first, when the camera is within
  320 m × view of their edge; unload beyond 520 m × view. One job at a time (HDD).
- Distance bands per object: HD visible `d < far`, where far = the stand-in's switch distance
  (max IDE draw distance of everything it replaces) so groups swap together; stand-ins visible
  `switch ≤ d < max(own draw, 2.5 × switch)`, and up close while their chunk isn't loaded.
  `+`/`-` scales everything (GTA's view-distance slider).
- Shared, refcounted GPU cache: models → meshes + materials (one material per texture, so Bevy
  batches), images freed when the last model using them unloads.
- Measured: whole city from 253 Broadway → 9 chunks loaded in ~35 s, 90–100 fps, ~500–800
  meshes drawn after culling (debug build of our code, optimised deps).

## Collision (`rage::bounds` — reverse-engineered, no public spec)
- WBN root +0x08 → bound; +0x04 u8 type: 0x0c composite, 0x0a geometry/BVH (only types met in
  the 732 map .wbn files). Composite: +0x80 child ptrs, +0x84 4×4 matrices (identity for map
  collision = world space), +0x90 u16 count. Geometry: +0x8C polygons, **+0x90 per-axis vertex
  scale** (+0x70 is the margin, not the scale), +0xA0 offset, +0xB0 i16×3 vertices,
  +0xC8/+0xCC vertex/polygon counts. Polygon 32 B: normal, area, u16 idx[4], i16 neighbour[4];
  triangle vs quad decided by which reading matches the stored area.
- Full scan: 732 files, 14.6M polygons → 24.3M triangles, 0 area mismatches, 0 unknown types.
- Viewer: each HD chunk's .wbn merged + grid-indexed off-thread (`collision.rs`, written by a
  worker subagent: ground height + sphere push-out, Ericson closest point). `F` toggles walk:
  eye 1.7 m, 4 / 9 m/s, gravity, Space jump, 0.6 m step-up, hovers until collision streams in.
  `LIBERTY_WALK=1` starts walking, `LIBERTY_AUTOWALK=<s>` holds W (scripted wall test).

## Animation (.wad, age::anim — reverse-engineered)
- pgDictionary<crAnimation>: +0x08 frames, +0x0C duration, +0x14 sequences, +0x1C name,
  +0x20 track descriptors. Channel: byte0 track type (0 pos, 1 rot, 5/6 mover pos/rot, 0x8x
  facial/extra), byte1 layout (0x10 vec3 of floats, 0x11 quat of 4 floats, 0x01 one quat),
  u16 bone ID, sub-channel ptrs, count @+0x14. Sub-channel encoding byte @+5: 0x01 raw floats,
  0x04 static float, 0x06 quantized (bits LSB-first, quantum, offset), 0x09 static quat by ptr,
  0x0B static quat inline. Full scan: 6,372 clips, 0 bad quaternions in 30.2M frames.
- Bone quaternions/positions are used as stored (verified: compose to stored abs, 0 error).
- Niko (ped.rs/pedanim.rs): move_player idle/walk/run/sprint blended by speed (speeds from
  mover tracks 1.7/5.1/7.4 m/s), start/stop/180-turn one-shots with root motion + cross-fades.
- Cars (eh@std.wad): F → run to the door (anchor = seat − get_in_ds root motion reversed),
  d_open_out_ds → get_in_ds → d_close_in_ds → sit_drive; out: get_out_ds → d_close_out_ds.
  Cars are skinned (doors swing: door_dside_f hinge, worker subagent). Shared car textures are
  in vehicles.img ehshare(_truck).wtd, NOT pc/textures/vehicle.wtd.
- Checking motion: --record <dir> --record-secs N + LIBERTY_PED_SCRIPT / LIBERTY_CAR_IN/OUT /
  LIBERTY_PED_CLIPS → ffmpeg (WSL) → watch_video. One screenshot can't show animation bugs.

## Next
1. Pedestrians (componentpeds.img + move_m@/move_f@ styles), then traffic.
2. Seated Niko steering (steer_l/_r), passenger side, jacking.
3. Night lighting/emissives (read FusionFix's shader work first), prop collision.
2. Far LOD (lodcull_*/slod) beyond the stand-ins; streaming meshes per object not per chunk.
3. Emissive/night shaders, glass, reflections on water; vehicle fragments with bones (cars).
4. Release/`play` build profile; upgrade Bevy 0.16 → current.

Refs: gtamods.com/wiki (IMG_archive, Cryptography, RSC), SparkIV / RageLib (C#, GPL — read, don't copy).
