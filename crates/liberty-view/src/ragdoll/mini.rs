//! S1 live mini player: a small window next to the real game. Whenever one of GTA IV's characters
//! goes limp (Euphoria), it shows IV's body (posed from its live links) and ours (the replay, driven by
//! the same live targets and gains) side by side, with the per-joint error, tilt and COM height.
//! Every episode is saved to `out/s1/live/<time>_<char>.txt` (the replay's flat format; replay it with
//! `liberty-view --replay <file>`, which gives the same numbers as `<file>.live.csv`).
//!
//! The data comes from `re/mglive.py` (read-only memory reading, memgrab2's way), which this starts
//! hidden and which connects back on 127.0.0.1. The window never takes focus (the player is playing): it
//! shows without activating and clicks don't activate it; no sound.
//!
//! Run: `liberty-view --mini`. Switches: LIBERTY_MINI_FAKE=<episode.txt> (no game: the reader plays a
//! recorded episode over and over), LIBERTY_MINI_TOP=0 (not always on top), LIBERTY_PYTHON=<exe>.

use super::replay::{self, from_nm_v, Map, Measure, Rec, Replay, JOINTS, LINKS, NAMES};
use super::*;
use bevy::winit::{UpdateMode, WinitSettings};
use std::io::{BufRead, Write as _};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TITLE: &str = "Liberty mini player";
/// An episode ends when its character sends nothing for this long.
const QUIET: Duration = Duration::from_millis(1000);

/// The checklist (like the 1 Oct session window): what S1 needs from the real game. F9 next, F8 back
/// (read with GetAsyncKeyState, so they work while the game has focus). Each episode is saved under
/// the step it was recorded in. (tag, title, episodes wanted, what to do)
const STEPS: [(&str, &str, usize, &str); 8] = [
    ("shove_walk", "Walk into people", 5, "A pavement. Walk into one person at a time;
let each one finish stumbling first."),
    ("shove_run", "Run into people", 5, "The same, running. Staying up and falling
are both wanted."),
    ("shot_body", "Shoot: body, from the front", 3, "Pistol, one shot in the chest,
one person at a time."),
    ("shot_leg", "Shoot: leg", 3, "Pistol, one shot in a thigh."),
    ("shot_back", "Shoot: back", 3, "Pistol, one shot in the back."),
    ("stairs", "Shove on stairs", 3, "Steps (a subway entrance, a building front):
walk into someone standing on them."),
    ("wall", "Shove by a wall or railing", 3, "Walk into someone standing next to
a wall or a railing."),
    ("car", "Car hits", 4, "In a car: hit people at walking pace (2),
then at about 30 km/h (2)."),
];

enum Msg {
    Status(String),
    Tick(u32, String),
    /// Still in Euphoria, lying still: no new tick.
    Alive(u32),
}

struct Episode {
    ch: u32,
    rp: Replay,
    /// The reader's clock at the episode's first tick (ticks are saved from 0).
    t0: f32,
    file: std::io::BufWriter<std::fs::File>,
    csv: std::io::BufWriter<std::fs::File>,
    path: PathBuf,
    seen: Instant,
    /// The checklist step it was recorded in.
    step: usize,
    /// Its last real tick (not a keep-alive): the window shows the one that moved last.
    moved: Instant,
}

#[derive(Resource)]
struct Mini {
    def: RagdollDef,
    rx: Mutex<Receiver<Msg>>,
    status: String,
    live: Vec<Episode>,
    /// The episode on screen: the newest live one, else the last that ended.
    ended: Option<(Episode, String)>,
    dir: PathBuf,
    count: usize,
    /// Checklist: the current step, and episodes saved per step this run.
    step: usize,
    done: [usize; STEPS.len()],
}

#[derive(Component)]
struct Hud;

pub fn run(def: RagdollDef) -> anyhow::Result<()> {
    let dir = PathBuf::from("out/s1/live");
    std::fs::create_dir_all(&dir)?;
    let (tx, rx) = std::sync::mpsc::channel();
    start_reader(tx)?;
    let top = std::env::var("LIBERTY_MINI_TOP").map_or(true, |v| v != "0");
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::audio::AudioPlugin>()
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: TITLE.into(),
                    resolution: (720.0_f32, 460.0_f32).into(),
                    // shown later without activating (see `no_focus`), so the game keeps its focus
                    visible: false,
                    focused: false,
                    window_level: if top { bevy::window::WindowLevel::AlwaysOnTop } else { bevy::window::WindowLevel::Normal },
                    position: WindowPosition::At(IVec2::new(40, 40)),
                    ..default()
                }),
                ..default()
            }),
    )
    // continuous (a reactive loop stalls on a window winit showed itself), capped at ~30 frames a
    // second by `pace` so it takes little from the game
    .insert_resource(WinitSettings { focused_mode: UpdateMode::Continuous, unfocused_mode: UpdateMode::Continuous })
    .insert_resource(ClearColor(Color::srgb(0.08, 0.08, 0.09)))
    .insert_resource(Mini { def, rx: Mutex::new(rx), status: "starting the reader ...".into(), live: Vec::new(), ended: None, dir, count: 0, step: 0, done: [0; STEPS.len()] })
    .add_systems(Startup, setup)
    .add_systems(Update, (no_focus, keys, take_ticks, draw, pace).chain());
    app.run();
    Ok(())
}

/// Start `re/mglive.py` hidden; it connects back here and streams lines into `tx`.
fn start_reader(tx: Sender<Msg>) -> anyhow::Result<()> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let script = ["re/mglive.py", concat!(env!("CARGO_MANIFEST_DIR"), "/../../re/mglive.py")]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("re/mglive.py not found (run from the Liberty folder)"))?;
    let python = std::env::var("LIBERTY_PYTHON").unwrap_or_else(|_| std::env::var("LOCALAPPDATA").unwrap_or_default() + r"\Programs\Python\Python312\python.exe");
    let mut cmd = std::process::Command::new(python);
    cmd.arg(&script).arg("--port").arg(port.to_string());
    if let Ok(f) = std::env::var("LIBERTY_MINI_FAKE") {
        cmd.arg("--fake").arg(f);
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console
    }
    let mut child = cmd.spawn()?;
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        let code = child.wait().map_or(-1, |s| s.code().unwrap_or(-1));
        let _ = tx2.send(Msg::Status(format!("the reader stopped (exit code {code})")));
    });
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let _ = tx.send(Msg::Status("reader connected".into()));
        for line in std::io::BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let msg = if let Some(s) = line.strip_prefix('#') {
                Msg::Status(s.trim().to_string())
            } else {
                let Some((ch, rest)) = line.split_once(' ') else { continue };
                let Ok(ch) = u32::from_str_radix(ch, 16) else { continue };
                if rest == "~" {
                    Msg::Alive(ch)
                } else {
                    Msg::Tick(ch, rest.to_string())
                }
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    Ok(())
}

fn setup(mut commands: Commands) {
    commands.spawn((Camera3d::default(), Transform::from_xyz(-1.0, 1.2, 3.9).looking_at(Vec3::new(-1.0, 0.8, 0.0), Vec3::Y)));
    commands.spawn((
        Hud,
        Text::new(""),
        TextFont { font_size: 12.0, ..default() },
        TextColor(Color::srgb(0.85, 0.85, 0.85)),
        Node { position_type: PositionType::Absolute, top: Val::Px(6.0), left: Val::Px(8.0), ..default() },
    ));
}

#[cfg(windows)]
mod win {
    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        pub fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
        pub fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
        pub fn ShowWindowAsync(hwnd: isize, cmd: i32) -> i32;
        pub fn GetAsyncKeyState(key: i32) -> i16;
    }
    pub const GWL_EXSTYLE: i32 = -20;
    pub const WS_EX_NOACTIVATE: isize = 0x0800_0000;
    pub const SW_SHOWNOACTIVATE: i32 = 4;
}

/// Once the window exists: mark it never-activate and show it without taking the focus.
fn no_focus(mut done: Local<bool>, windows: Query<(), With<bevy::window::RawHandleWrapper>>) {
    if *done || windows.is_empty() {
        return;
    }
    *done = true;
    // from another thread: these send messages to the window, which winit must not get mid-frame
    #[cfg(windows)]
    std::thread::spawn(|| unsafe {
        let title: Vec<u16> = TITLE.encode_utf16().chain([0]).collect();
        let h = win::FindWindowW(std::ptr::null(), title.as_ptr());
        info!("mini: window {h:#x}, shown without focus");
        if h != 0 {
            let ex = win::GetWindowLongPtrW(h, win::GWL_EXSTYLE);
            win::SetWindowLongPtrW(h, win::GWL_EXSTYLE, ex | win::WS_EX_NOACTIVATE);
            win::ShowWindowAsync(h, win::SW_SHOWNOACTIVATE);
        }
    });
}

/// F9 / F8: next / previous checklist step, whichever window has focus (asks the key state, no hook).
fn keys(mut mini: ResMut<Mini>, mut held: Local<[bool; 2]>) {
    #[cfg(windows)]
    for (i, (vk, d)) in [(0x78, 1isize), (0x77, -1)].into_iter().enumerate() {
        let down = unsafe { win::GetAsyncKeyState(vk) } as u16 & 0x8000 != 0;
        if down && !held[i] {
            mini.step = (mini.step as isize + d).clamp(0, STEPS.len() as isize - 1) as usize;
            info!("mini: step {}", STEPS[mini.step].0);
        }
        held[i] = down;
    }
}

/// Seconds since 1970 as a UTC stamp, `20261008-191500`.
fn stamp() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    // civil date from days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

/// Re-time a tick line from the episode start (the first number), keeping the rest as sent.
fn retime(rest: &str, t0: f32) -> Option<(String, Rec)> {
    let (t, tail) = rest.split_once(' ')?;
    let t: f32 = t.parse().ok()?;
    let line = format!("{} {tail}", t - t0);
    let rec = replay::parse_line(&line)?;
    Some((line, rec))
}

fn take_ticks(mut mini: ResMut<Mini>, mut n: Local<u32>) {
    *n += 1;
    if *n % 30 == 0 && std::env::var("LIBERTY_MINI_DEBUG").is_ok() {
        info!("mini: frame {}", *n);
    }
    let mini = &mut *mini;
    let msgs: Vec<Msg> = mini.rx.lock().unwrap().try_iter().collect();
    for msg in msgs {
        match msg {
            Msg::Status(s) => {
                info!("mini: {s}");
                mini.status = s;
            }
            Msg::Alive(ch) => {
                if let Some(ep) = mini.live.iter_mut().find(|e| e.ch == ch) {
                    ep.seen = Instant::now();
                }
            }
            Msg::Tick(ch, rest) => {
                if let Some(i) = mini.live.iter().position(|e| e.ch == ch) {
                    let ep = &mut mini.live[i];
                    let Some((line, rec)) = retime(&rest, ep.t0) else { continue };
                    // IV hands the same character to another ped: the body jumps; a new episode
                    if (rec.com - ep.rp.cur.com).length() <= 1.0 {
                        let _ = writeln!(ep.file, "{line}");
                        let m = ep.rp.feed(&mini.def, rec);
                        let _ = writeln!(ep.csv, "{}", m.csv_row());
                        ep.seen = Instant::now();
                        ep.moved = ep.seen;
                        continue;
                    }
                    finish(mini, i);
                }
                // a new episode: our body starts in this tick's pose, on flat ground under IV's feet
                let Some(t0) = rest.split_once(' ').and_then(|(t, _)| t.parse::<f32>().ok()) else { continue };
                let Some((line, rec)) = retime(&rest, t0) else { continue };
                let ground = replay::ground_under(std::slice::from_ref(&rec));
                let path = mini.dir.join(format!("{}_{}_{ch:x}.txt", stamp(), STEPS[mini.step].0));
                let (Ok(f), Ok(c)) = (std::fs::File::create(&path), std::fs::File::create(path.with_extension("live.csv"))) else {
                    mini.status = format!("can't write {}", path.display());
                    continue;
                };
                let mut file = std::io::BufWriter::new(f);
                let mut csv = std::io::BufWriter::new(c);
                let _ = writeln!(file, "# liberty live episode char={ch:x} utc={} step={} ground={ground:?}", stamp(), STEPS[mini.step].0);
                let _ = writeln!(file, "{line}");
                let _ = writeln!(csv, "{}", Measure::csv_head());
                let rp = Replay::new(&mini.def, &rec, ground, false);
                info!("mini: episode {} (character {ch:x})", path.display());
                mini.count += 1;
                let step = mini.step;
                mini.live.push(Episode { ch, rp, t0, file, csv, path, seen: Instant::now(), step, moved: Instant::now() });
            }
        }
    }
    // episodes whose character went quiet: save and keep the last one on screen
    while let Some(i) = mini.live.iter().position(|e| e.seen.elapsed() > QUIET) {
        finish(mini, i);
    }
}

/// Save an episode and keep it for the screen.
fn finish(mini: &mut Mini, i: usize) {
    {
        let mut ep = mini.live.remove(i);
        let _ = ep.file.flush();
        let _ = ep.csv.flush();
        info!("mini: episode ended after {} ticks", ep.rp.ticks() + 1);
        if ep.rp.ticks() >= 10 {
            mini.done[ep.step] += 1;
        }
        let note = format!("ended, {} ticks: saved {}", ep.rp.ticks() + 1, ep.path.file_name().map_or(String::new(), |n| n.to_string_lossy().into()));
        mini.ended = Some((ep, note));
    }
}

/// Sleep out the rest of a 1/30 s frame.
fn pace(mut last: Local<Option<Instant>>) {
    let frame = Duration::from_millis(33);
    if let Some(t) = *last {
        if let Some(rest) = frame.checked_sub(t.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    *last = Some(Instant::now());
}

const IV_COLOR: Color = Color::srgb(1.0, 0.62, 0.2);
const OUR_COLOR: Color = Color::srgb(0.3, 0.8, 1.0);

/// IV part tree for drawing (child, parent), IV part order (replay.rs).
const BONES: [(usize, usize); 20] = [
    (1, 0), (2, 1), (3, 2), (4, 0), (5, 4), (6, 5), (7, 0), (8, 7), (9, 8), (10, 9), (11, 10), (12, 11),
    (13, 10), (14, 13), (15, 14), (16, 15), (17, 10), (18, 17), (19, 18), (20, 19),
];

fn draw(mini: Res<Mini>, mut gizmos: Gizmos, mut hud: Query<&mut Text, With<Hud>>) {
    let shown = mini.live.iter().max_by_key(|e| e.moved).map(|e| (e, "live".to_string())).or_else(|| mini.ended.as_ref().map(|(e, n)| (e, n.clone())));
    // floor
    for i in -6..=6 {
        let x = i as f32 * 0.25;
        gizmos.line(Vec3::new(x, 0.0, -1.0), Vec3::new(x, 0.0, 1.0), Color::srgb(0.22, 0.22, 0.24));
    }
    for i in -4..=4 {
        let z = i as f32 * 0.25;
        gizmos.line(Vec3::new(-1.5, 0.0, z), Vec3::new(1.5, 0.0, z), Color::srgb(0.22, 0.22, 0.24));
    }
    let (_, title, want, how) = STEPS[mini.step];
    let mut text = format!(
        "STEP {}/{}  {title}   {} of {want}   (F9 next, F8 back)\n{how}\n\n{}   episodes: {}\n",
        mini.step + 1,
        STEPS.len(),
        mini.done[mini.step],
        mini.status,
        mini.count
    );
    if let Some((ep, state)) = shown {
        let def = &mini.def;
        let rp = &ep.rp;
        let ground_y = from_nm_v(Vec3::new(0.0, 0.0, rp.ground_gta)).y;
        // both bodies turned the same way: IV's start frame's right along the screen's x
        let yaw = replay::rec_frame(&rp.rec0).map_or(0.0, |f| f.x_axis.z.atan2(f.x_axis.x));
        let turn = Quat::from_rotation_y(yaw + 0.5);
        // IV: links from its live record, centred on its COM over the ground
        let r = &rp.cur;
        let rc = replay::rec_rel_com(r);
        let iv: Vec<Vec3> = (0..LINKS).map(|k| from_nm_v(r.com + r.links[k].pos - rc)).collect();
        let c = from_nm_v(r.com);
        let iv: Vec<Vec3> = iv.iter().map(|p| turn * Vec3::new(p.x - c.x, p.y - ground_y, p.z - c.z) + Vec3::new(-0.6, 0.0, 0.0)).collect();
        // ours: each IV part's link COM
        let oc = rp.sim.whole_com();
        let ours: Vec<Option<Vec3>> = (0..LINKS)
            .map(|k| rp.sim.iv_part_com(def, k).map(|p| turn * Vec3::new(p.x - oc.x, p.y - ground_y, p.z - oc.z) + Vec3::new(0.6, 0.0, 0.0)))
            .collect();
        for (a, b) in BONES {
            gizmos.line(iv[a], iv[b], IV_COLOR);
            if let (Some(pa), Some(pb)) = (ours[a], ours[b]) {
                gizmos.line(pa, pb, OUR_COLOR);
            }
        }
        for k in 0..LINKS {
            gizmos.sphere(Isometry3d::from_translation(iv[k]), 0.025, IV_COLOR);
            if let Some(p) = ours[k] {
                gizmos.sphere(Isometry3d::from_translation(p), 0.025, OUR_COLOR);
            }
        }
        text += &format!("character {:x}: {state}\n", ep.ch);
        if let Some(m) = &rp.last {
            text += &format!(
                "t {:5.2} s            IV (orange)   ours (blue)\ntilt                {:6.1} deg   {:6.1} deg\nCOM height          {:6.2} m     {:6.2} m\nCOM moved           {:6.2} m     {:6.2} m\njoint error, now / RMS so far (deg):\n",
                m.t, m.iv_tilt, m.our_tilt, m.iv_com_z, m.our_com_z, m.iv_dx, m.our_dx
            );
            let rms = rp.rms();
            let cells: Vec<String> = (0..JOINTS)
                .filter(|&k| !matches!(rp.map[k], Map::None))
                .map(|k| format!("{:10}{:4.0}/{:<4.0}", NAMES[k], m.err(&rp.map, k).unwrap_or(0.0).to_degrees(), rms[k].iter().cloned().fold(0.0, f64::max)))
                .collect();
            for row in cells.chunks(2) {
                text += &row.join("  ");
                text.push('\n');
            }
        }
    } else {
        text += "\nshove or shoot a ped in the game: both bodies show here";
    }
    if let Ok(mut t) = hud.single_mut() {
        if t.0 != text {
            t.0 = text;
        }
    }
}
