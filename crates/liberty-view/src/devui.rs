//! The dev panel (egui), Liberty's own look: a status line along the top, a dock of buttons on
//! the left that opens one drawer at a time (Time, Places, Layers, Settings, Keys), and a console
//! that drops down from the top (`). Slate panels, streetlight-amber accent.
//!
//! Only in interactive runs. `--shot`, `--record`, `--testlab` and LIBERTY_NO_INPUT leave it out,
//! so scripted runs and lab pictures look as before. LIBERTY_DEVUI=<time|places|layers|settings|
//! keys>,console,quit opens those at start (with `--shot`, a picture of the panel itself).
//!
//! Keys: ` console · F1 keys · F2 hide everything · Esc closes what's open, and with nothing open
//! asks before quitting. While the pointer is over a panel or a text box has the keyboard, the
//! game doesn't see that input (bevy_egui's absorb system).

use std::collections::VecDeque;
use std::sync::Mutex;

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode};
use bevy_egui::{EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, egui};
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke, pos2, vec2};

use crate::stream::City;
use crate::timecyc::TimeCycle;
use crate::{Fly, HudText, Start};

const BG: Color32 = Color32::from_rgba_premultiplied(15, 17, 21, 238);
const BG2: Color32 = Color32::from_rgb(27, 30, 36);
const BG3: Color32 = Color32::from_rgb(38, 42, 50);
const EDGE: Color32 = Color32::from_rgb(52, 57, 68);
const TEXT: Color32 = Color32::from_rgb(232, 234, 238);
const DIM: Color32 = Color32::from_rgb(138, 145, 158);
/// Sodium streetlight.
const AMBER: Color32 = Color32::from_rgb(242, 163, 58);
const AMBER_HI: Color32 = Color32::from_rgb(255, 186, 92);
const ON_AMBER: Color32 = Color32::from_rgb(28, 20, 8);
const WARN: Color32 = Color32::from_rgb(240, 200, 96);
const ERR: Color32 = Color32::from_rgb(235, 96, 96);
const R: u8 = 6;

/// GTA IV's clock: 1 game minute per 2 real seconds.
const BASE_RATE: f32 = 1.0 / 120.0;
const DEFAULT_FOV: f32 = std::f32::consts::FRAC_PI_4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drawer {
    Time,
    Places,
    Layers,
    Settings,
    Keys,
}

/// A saved camera: GTA position, heading (0 = north, 90 = west) and pitch, in degrees.
#[derive(Clone)]
struct Place {
    name: String,
    pos: [f32; 3],
    heading: f32,
    pitch: f32,
}

#[derive(Resource)]
pub struct DevUi {
    drawer: Option<Drawer>,
    console: bool,
    quit: bool,
    /// F2: everything hidden.
    hidden: bool,
    status: bool,
    places: Vec<Place>,
    place_name: String,
    search: String,
    input: String,
    history: Vec<String>,
    hist_at: Option<usize>,
    focus_input: bool,
    started: bool,
}

pub struct DevUiPlugin {
    list: String,
}

impl DevUi {
    /// Something on screen needs the mouse (a drawer, the console, the quit question): looklock.rs lets go.
    pub fn wants_cursor(&self) -> bool {
        !self.hidden && (self.drawer.is_some() || self.console || self.quit)
    }
}

impl DevUiPlugin {
    /// `list`: LIBERTY_DEVUI's comma list of what to open at start.
    pub fn from_list(list: &str) -> DevUiPlugin {
        DevUiPlugin { list: list.to_ascii_lowercase() }
    }
}

impl Plugin for DevUiPlugin {
    fn build(&self, app: &mut App) {
        let has = |w: &str| self.list.split(',').any(|s| s.trim() == w);
        let drawer = [("time", Drawer::Time), ("places", Drawer::Places), ("layers", Drawer::Layers), ("settings", Drawer::Settings), ("keys", Drawer::Keys)]
            .into_iter()
            .find(|(n, _)| has(n))
            .map(|(_, d)| d);
        app.add_plugins(EguiPlugin::default())
            .insert_resource(DevUi {
                drawer,
                console: has("console"),
                quit: has("quit"),
                hidden: false,
                status: true,
                places: load_places(),
                place_name: String::new(),
                search: String::new(),
                input: String::new(),
                history: Vec::new(),
                hist_at: None,
                focus_input: has("console"),
                started: false,
            })
            .add_systems(EguiPrimaryContextPass, ui);
    }
}

// ---------------------------------------------------------------------------------------------
// The game log, for the console: a tracing layer copies every info/warn/error line here.

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Info,
    Warn,
    Error,
    Cmd,
    Out,
}

static LOG: Mutex<VecDeque<(Kind, String)>> = Mutex::new(VecDeque::new());

fn push(kind: Kind, line: impl Into<String>) {
    if let Ok(mut log) = LOG.lock() {
        if log.len() >= 800 {
            log.pop_front();
        }
        log.push_back((kind, line.into()));
    }
}

fn out(line: impl Into<String>) {
    push(Kind::Out, line);
}

fn err(line: impl Into<String>) {
    push(Kind::Error, line);
}

struct ConsoleLayer;

struct Message(String, String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        } else if !field.name().starts_with("log.") {
            // (the `log` crate's bridge adds log.target / log.file / ...: noise here)
            self.1 += &format!(" {}={value:?}", field.name());
        }
    }
}

impl<S: tracing::Subscriber> bevy::log::tracing_subscriber::Layer<S> for ConsoleLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: bevy::log::tracing_subscriber::layer::Context<'_, S>) {
        let kind = match *event.metadata().level() {
            tracing::Level::ERROR => Kind::Error,
            tracing::Level::WARN => Kind::Warn,
            tracing::Level::INFO => Kind::Info,
            _ => return,
        };
        let mut m = Message(String::new(), String::new());
        event.record(&mut m);
        push(kind, m.0 + &m.1);
    }
}

/// For `LogPlugin::custom_layer`.
pub fn log_layer(_: &mut App) -> Option<bevy::log::BoxedLayer> {
    Some(Box::new(ConsoleLayer))
}

// ---------------------------------------------------------------------------------------------
// Saved places: liberty_places.txt beside the exe (name, x, y, z, heading, pitch; tab separated).

fn places_path() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("liberty_places.txt"))
}

/// A camera at `eye` looking at `target` (GTA metres).
fn looking(name: &str, eye: [f32; 3], target: [f32; 3]) -> Place {
    let d = [target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]];
    Place {
        name: name.into(),
        pos: eye,
        heading: (-d[0]).atan2(d[1]).to_degrees().rem_euclid(360.0) + 0.0, // + 0.0: no "-0"
        pitch: d[2].atan2(d[0].hypot(d[1])).to_degrees(),
    }
}

fn load_places() -> Vec<Place> {
    let Some(text) = places_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        // the two lab spots
        return vec![
            looking("Tennis courts", [1466.0, 721.0, 46.0], [1497.5, 750.6, 27.4]),
            looking("Star Junction", [-206.0, 318.0, 42.0], [-206.0, 379.0, 15.0]),
        ];
    };
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let n = |i: usize| f.get(i)?.trim().parse::<f32>().ok();
            Some(Place { name: f.first()?.to_string(), pos: [n(1)?, n(2)?, n(3)?], heading: n(4)?, pitch: n(5)? })
        })
        .collect()
}

fn save_places(places: &[Place]) {
    let Some(path) = places_path() else { return };
    let text: String = places
        .iter()
        .map(|p| format!("{}\t{:.2}\t{:.2}\t{:.2}\t{:.1}\t{:.1}\n", p.name.replace('\t', " "), p.pos[0], p.pos[1], p.pos[2], p.heading, p.pitch))
        .collect();
    if let Err(e) = std::fs::write(&path, text) {
        err(format!("couldn't save places to {}: {e}", path.display()));
    }
}

// ---------------------------------------------------------------------------------------------
// What the panel reads and changes.

#[derive(SystemParam)]
pub struct Game<'w, 's> {
    cam: Query<'w, 's, (&'static mut Transform, &'static mut Fly, &'static mut Projection), With<Camera3d>>,
    tc: Option<ResMut<'w, TimeCycle>>,
    city: Option<ResMut<'w, City>>,
    start: Option<Res<'w, Start>>,
    window: Query<'w, 's, &'static mut Window, With<PrimaryWindow>>,
    sun: Query<'w, 's, &'static mut DirectionalLight>,
    diag: Res<'w, DiagnosticsStore>,
    hud_text: Query<'w, 's, &'static mut Visibility, With<HudText>>,
    hud: Query<'w, 's, &'static mut Visibility, (With<Node>, Without<ChildOf>, Without<HudText>)>,
    exit: EventWriter<'w, AppExit>,
    roads: Option<ResMut<'w, crate::traffic::Roads>>,
    peds_on: Option<ResMut<'w, crate::peds::PedsOn>>,
}

impl Game<'_, '_> {
    /// Camera position (GTA), heading and pitch (degrees).
    fn pose(&self) -> Option<([f32; 3], f32, f32)> {
        let (t, fly, _) = self.cam.single().ok()?;
        let p = t.translation;
        Some(([p.x, -p.z, p.y], fly.yaw.to_degrees().rem_euclid(360.0), fly.pitch.to_degrees()))
    }

    fn mode(&self) -> &'static str {
        match self.cam.single() {
            Ok((_, f, _)) if f.driving.is_some() => "DRIVE",
            Ok((_, f, _)) if f.walking => "WALK",
            _ => "FLY",
        }
    }

    fn teleport(&mut self, pos: [f32; 3], heading: Option<f32>, pitch: Option<f32>) {
        let Ok((mut t, mut fly, _)) = self.cam.single_mut() else { return };
        if fly.driving.is_some() {
            err("get out of the car first (F)");
            return;
        }
        fly.walking = false;
        fly.vel_y = 0.0;
        if let Some(h) = heading {
            fly.yaw = h.to_radians();
        }
        if let Some(p) = pitch {
            fly.pitch = p.to_radians().clamp(-1.55, 1.55);
        }
        t.translation = Vec3::new(pos[0], pos[2], -pos[1]);
        t.rotation = Quat::from_euler(EulerRot::YXZ, fly.yaw, fly.pitch, 0.0);
    }

    fn home(&mut self) {
        let Some((eye, target)) = self.start.as_ref().map(|s| (s.eye, s.target)) else { return };
        let (yaw, pitch, _) = Transform::from_translation(eye).looking_at(target, Vec3::Y).rotation.to_euler(EulerRot::YXZ);
        self.teleport([eye.x, -eye.z, eye.y], Some(yaw.to_degrees()), Some(pitch.to_degrees()));
    }

    fn face_north(&mut self) {
        if let Some((p, _, pitch)) = self.pose() {
            self.teleport(p, Some(0.0), Some(pitch));
        }
    }

    fn fov(&self) -> f32 {
        match self.cam.single().map(|c| c.2) {
            Ok(Projection::Perspective(p)) => p.fov,
            _ => DEFAULT_FOV,
        }
    }

    fn set_fov(&mut self, fov: f32) {
        if let Ok((_, _, mut proj)) = self.cam.single_mut() {
            if let Projection::Perspective(p) = proj.as_mut() {
                p.fov = fov.clamp(20f32.to_radians(), 110f32.to_radians());
            }
        }
    }

    fn set_walking(&mut self, walking: bool) {
        if let Ok((_, mut fly, _)) = self.cam.single_mut() {
            if fly.driving.is_none() && self.city.is_some() && fly.walking != walking {
                fly.walking = walking;
                fly.vel_y = 0.0;
            }
        }
    }

    fn shadows(&self) -> bool {
        self.sun.iter().any(|l| l.shadows_enabled)
    }

    fn set_shadows(&mut self, on: bool) {
        for mut l in &mut self.sun {
            l.shadows_enabled = on;
        }
    }
}

// ---------------------------------------------------------------------------------------------

fn ui(mut contexts: EguiContexts, mut d: ResMut<DevUi>, mut g: Game, mut egui_settings: ResMut<EguiGlobalSettings>) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    if !d.started {
        d.started = true;
        egui_settings.enable_absorb_bevy_input_system = true;
        style(&ctx);
        // the status line replaces the old one-line HUD text
        for mut v in &mut g.hud_text {
            *v = Visibility::Hidden;
        }
    }

    // keys the panel owns (read from egui, so they work while a text box has the keyboard)
    let (backtick, f1, f2, esc) = ctx.input(|i| {
        (i.key_pressed(egui::Key::Backtick), i.key_pressed(egui::Key::F1), i.key_pressed(egui::Key::F2), i.key_pressed(egui::Key::Escape))
    });
    if backtick {
        d.console = !d.console;
        d.focus_input = d.console;
    }
    if f1 {
        flip(&mut d, Drawer::Keys);
    }
    if f2 {
        d.hidden = !d.hidden;
    }
    if esc {
        if d.quit {
            d.quit = false;
        } else if d.console {
            d.console = false;
        } else if d.drawer.is_some() {
            d.drawer = None;
        } else {
            d.quit = true;
        }
    }
    if d.hidden {
        return Ok(());
    }

    if d.status {
        status_line(&ctx, &mut g);
    }
    dock(&ctx, &mut d, &mut g);
    if let Some(drawer) = d.drawer {
        drawer_panel(&ctx, &mut d, &mut g, drawer);
    }
    if d.console {
        console(&ctx, &mut d, &mut g);
    }
    if d.quit {
        quit_prompt(&ctx, &mut d, &mut g);
    }
    Ok(())
}

fn flip(d: &mut DevUi, which: Drawer) {
    d.drawer = if d.drawer == Some(which) { None } else { Some(which) };
}

fn style(ctx: &egui::Context) {
    use egui::{FontFamily, TextStyle};
    let mut s = (*ctx.style()).clone();
    s.text_styles = [
        (TextStyle::Heading, FontId::new(16.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
    ]
    .into();
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.window_fill = BG;
    v.panel_fill = BG;
    v.window_stroke = pen(1.0, EDGE);
    v.window_corner_radius = CornerRadius::same(R);
    v.menu_corner_radius = CornerRadius::same(R);
    v.extreme_bg_color = BG2;
    v.text_edit_bg_color = Some(BG2);
    v.selection.bg_fill = AMBER.gamma_multiply(0.45);
    v.selection.stroke = pen(1.0, AMBER_HI);
    for (w, fill) in [(&mut v.widgets.inactive, BG2), (&mut v.widgets.hovered, BG3), (&mut v.widgets.active, BG3), (&mut v.widgets.open, BG3)] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = CornerRadius::same(4);
        w.bg_stroke = pen(1.0, EDGE);
        w.fg_stroke = pen(1.0, TEXT);
        w.expansion = 0.0;
    }
    v.widgets.hovered.bg_stroke = pen(1.0, AMBER);
    v.widgets.noninteractive.bg_stroke = pen(1.0, EDGE);
    v.widgets.noninteractive.fg_stroke = pen(1.0, TEXT);
    s.visuals = v;
    s.spacing.item_spacing = vec2(8.0, 6.0);
    s.spacing.button_padding = vec2(10.0, 4.0);
    s.spacing.interact_size.y = 24.0;
    ctx.set_style(s);
}

// ---------------------------------------------------------------------------------------------
// Widgets.

fn pen(width: f32, col: Color32) -> Stroke {
    Stroke::new(width, col)
}

fn rr(r: u8) -> CornerRadius {
    CornerRadius::same(r)
}

fn frame() -> egui::Frame {
    egui::Frame::new().fill(BG).stroke(pen(1.0, EDGE)).corner_radius(R).inner_margin(Margin::same(14))
}

/// Small uppercase section heading with a rule after it.
fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(text.to_uppercase()).color(DIM).size(11.0).strong());
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::hover());
        ui.painter().line_segment([r.left_center() + vec2(4.0, 0.0), r.right_center()], pen(1.0, EDGE));
    });
}

/// Label on the left, control on the right.
fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), add);
    });
}

/// Sliding on/off switch.
fn switch(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let p = ui.painter();
    let track = if *on { AMBER } else { BG3 };
    p.rect_filled(rect, rr(10), track);
    if resp.hovered() {
        p.rect_stroke(rect, rr(10), pen(1.0, AMBER_HI), egui::StrokeKind::Inside);
    }
    let x = egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), t);
    p.circle_filled(pos2(x, rect.center().y), 7.0, if *on { ON_AMBER } else { DIM });
    resp.clicked()
}

/// A row of options, the chosen one filled amber.
fn choice(ui: &mut egui::Ui, items: &[&str], sel: &mut usize) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, label) in items.iter().enumerate() {
            let on = *sel == i;
            let b = egui::Button::new(RichText::new(*label).color(if on { ON_AMBER } else { TEXT }).size(13.0))
                .fill(if on { AMBER } else { BG2 })
                .corner_radius(rr(4))
                .min_size(vec2(0.0, 24.0));
            if ui.add(b).clicked() && !on {
                *sel = i;
                changed = true;
            }
        }
    });
    changed
}

/// Thin track, amber up to the handle; `to_t`/`from_t` map the value to 0..1 and back.
fn slide(ui: &mut egui::Ui, v: &mut f32, width: f32, to_t: impl Fn(f32) -> f32, from_t: impl Fn(f32) -> f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 20.0), Sense::click_and_drag());
    let (x0, x1, y) = (rect.left() + 6.0, rect.right() - 6.0, rect.center().y);
    let mut changed = false;
    if let Some(at) = resp.interact_pointer_pos() {
        let nv = from_t(((at.x - x0) / (x1 - x0)).clamp(0.0, 1.0));
        if nv != *v {
            *v = nv;
            changed = true;
        }
    }
    let kx = x0 + to_t(*v).clamp(0.0, 1.0) * (x1 - x0);
    let p = ui.painter();
    p.line_segment([pos2(x0, y), pos2(x1, y)], pen(3.0, BG3));
    p.line_segment([pos2(x0, y), pos2(kx, y)], pen(3.0, AMBER));
    let hot = resp.hovered() || resp.dragged();
    p.rect_filled(egui::Rect::from_center_size(pos2(kx, y), vec2(10.0, if hot { 18.0 } else { 16.0 })), rr(3), if hot { AMBER_HI } else { TEXT });
    changed
}

fn slide_lin(ui: &mut egui::Ui, v: &mut f32, lo: f32, hi: f32, width: f32) -> bool {
    slide(ui, v, width, |v| (v - lo) / (hi - lo), |t| lo + t * (hi - lo))
}

fn slide_log(ui: &mut egui::Ui, v: &mut f32, lo: f32, hi: f32, width: f32) -> bool {
    let (a, b) = (lo.ln(), hi.ln());
    slide(ui, v, width, |v| (v.max(lo).ln() - a) / (b - a), |t| (a + t * (b - a)).exp())
}

/// Slider + typed number after a label.
#[allow(clippy::too_many_arguments)]
fn number(ui: &mut egui::Ui, label: &str, v: &mut f32, lo: f32, hi: f32, log: bool, decimals: usize, suffix: &str) -> bool {
    let mut changed = false;
    field(ui, label, |ui| {
        let typed = ui
            .add_sized([78.0, 24.0], egui::DragValue::new(v).range(lo..=hi).speed((hi - lo) / 400.0).fixed_decimals(decimals).suffix(suffix))
            .changed();
        let w = 140.0;
        let slid = if log { slide_log(ui, v, lo, hi, w) } else { slide_lin(ui, v, lo, hi, w) };
        changed = typed || slid;
    });
    changed
}

fn close_x(ui: &mut egui::Ui) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    let resp = resp.on_hover_text("Close (Esc)");
    if resp.hovered() {
        ui.painter().rect_filled(rect, rr(4), BG3);
    }
    let c = rect.center();
    let col = if resp.hovered() { AMBER_HI } else { DIM };
    for (a, b) in [(vec2(-4.5, -4.5), vec2(4.5, 4.5)), (vec2(-4.5, 4.5), vec2(4.5, -4.5))] {
        ui.painter().line_segment([c + a, c + b], pen(1.5, col));
    }
    resp.clicked()
}

fn button(ui: &mut egui::Ui, text: &str, primary: bool) -> bool {
    let b = egui::Button::new(RichText::new(text).color(if primary { ON_AMBER } else { TEXT }))
        .fill(if primary { AMBER } else { BG2 })
        .corner_radius(rr(4))
        .min_size(vec2(0.0, 26.0));
    ui.add(b).clicked()
}

fn nice_name(s: &str) -> String {
    let s = s.replace('_', " ").to_ascii_lowercase();
    let mut c = s.chars();
    c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
}

fn clock(h: f32) -> String {
    format!("{:02}:{:02}", h as u32, (h.fract() * 60.0) as u32)
}

// ---------------------------------------------------------------------------------------------
// Dock icons, drawn.

#[derive(Clone, Copy)]
enum Icon {
    Home,
    Time,
    Places,
    Layers,
    Settings,
    Keys,
    Console,
}

fn dock_button(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
    let resp = resp.on_hover_text(tip);
    let p = ui.painter();
    if on || resp.hovered() {
        p.rect_filled(rect, rr(5), BG3);
    }
    if on {
        p.rect_filled(egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 8.0), vec2(3.0, 24.0)), rr(1), AMBER);
    }
    let col = if on || resp.hovered() { AMBER_HI } else { TEXT };
    let c = rect.center();
    let s = pen(1.7, col);
    match icon {
        Icon::Home => {
            p.add(egui::Shape::line(vec![c + vec2(-8.0, 0.0), c + vec2(0.0, -7.5), c + vec2(8.0, 0.0)], s));
            p.add(egui::Shape::line(vec![c + vec2(-5.5, -2.0), c + vec2(-5.5, 7.0), c + vec2(5.5, 7.0), c + vec2(5.5, -2.0)], s));
        }
        Icon::Time => {
            p.circle_stroke(c, 8.0, s);
            p.line_segment([c, c + vec2(0.0, -5.5)], s);
            p.line_segment([c, c + vec2(4.0, 2.0)], s);
        }
        Icon::Places => {
            p.circle_stroke(c + vec2(0.0, -2.5), 5.5, s);
            p.add(egui::Shape::line(vec![c + vec2(-4.6, 0.5), c + vec2(0.0, 8.5), c + vec2(4.6, 0.5)], s));
            p.circle_filled(c + vec2(0.0, -2.5), 1.8, col);
        }
        Icon::Layers => {
            for dy in [-5.0, 0.0, 5.0] {
                p.add(egui::Shape::line(vec![c + vec2(-8.0, dy - 1.5), c + vec2(0.0, dy + 2.0), c + vec2(8.0, dy - 1.5)], s));
            }
        }
        Icon::Settings => {
            for (dy, kx) in [(-5.0, 3.0), (0.0, -3.0), (5.0, 1.5)] {
                p.line_segment([c + vec2(-8.0, dy), c + vec2(8.0, dy)], s);
                p.circle_filled(c + vec2(kx, dy), 2.3, col);
            }
        }
        Icon::Keys => {
            let r = egui::Rect::from_center_size(c, vec2(18.0, 12.0));
            p.rect_stroke(r, rr(2), s, egui::StrokeKind::Middle);
            for x in [-5.0, 0.0, 5.0] {
                p.circle_filled(c + vec2(x, -2.0), 1.1, col);
            }
            p.line_segment([c + vec2(-4.0, 2.5), c + vec2(4.0, 2.5)], s);
        }
        Icon::Console => {
            p.add(egui::Shape::line(vec![c + vec2(-7.0, -5.0), c + vec2(-2.0, 0.0), c + vec2(-7.0, 5.0)], s));
            p.line_segment([c + vec2(0.0, 6.0), c + vec2(7.0, 6.0)], s);
        }
    }
    resp.clicked()
}

// ---------------------------------------------------------------------------------------------
// The pieces.

/// One line along the top: mode (click: fly/walk), FPS, position, heading (click: face north),
/// time and weather, chunks.
fn status_line(ctx: &egui::Context, g: &mut Game) {
    let fps = g.diag.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|x| x.smoothed()).unwrap_or(0.0);
    egui::Area::new(egui::Id::new("status")).anchor(Align2::LEFT_TOP, [12.0, 12.0]).show(ctx, |ui| {
        egui::Frame::new().fill(BG).stroke(pen(1.0, EDGE)).corner_radius(R).inner_margin(Margin::symmetric(6, 4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let mode = g.mode();
                let tag = egui::Button::new(RichText::new(mode).color(ON_AMBER).size(12.0).strong()).fill(AMBER).corner_radius(rr(4));
                if ui.add(tag).on_hover_text("Fly / walk (G)").clicked() {
                    match mode {
                        "FLY" => g.set_walking(true),
                        "WALK" => g.set_walking(false),
                        _ => out("F gets out of the car"),
                    }
                }
                let mono = |t: String| RichText::new(t).monospace();
                ui.label(mono(format!("{fps:>3.0} fps")).color(DIM));
                if let Some((p, heading, _)) = g.pose() {
                    ui.label(mono(format!("{:.1}  {:.1}  {:.1}", p[0], p[1], p[2])));
                    let arrow = ["N", "NW", "W", "SW", "S", "SE", "E", "NE"][((heading + 22.5) / 45.0) as usize % 8];
                    let h = egui::Button::new(mono(format!("{arrow} {heading:.0}°"))).fill(BG2).corner_radius(rr(4));
                    if ui.add(h).on_hover_text("Face north").clicked() {
                        g.face_north();
                    }
                }
                if let Some(tc) = &g.tc {
                    ui.label(mono(clock(tc.hour)));
                    ui.label(RichText::new(nice_name(tc.weather_name())).color(DIM));
                }
                if let (Some(city), Ok((t, _, _))) = (&g.city, g.cam.single()) {
                    let busy = !city.idle(t.translation);
                    ui.label(mono(format!("{}/{} chunks", city.loaded(), city.chunks.len())).color(if busy { AMBER } else { DIM }));
                }
            });
        });
    });
}

fn dock(ctx: &egui::Context, d: &mut DevUi, g: &mut Game) {
    egui::Area::new(egui::Id::new("dock")).anchor(Align2::LEFT_CENTER, [12.0, 0.0]).show(ctx, |ui| {
        egui::Frame::new().fill(BG).stroke(pen(1.0, EDGE)).corner_radius(R).inner_margin(Margin::same(4)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if dock_button(ui, Icon::Home, false, "Back to the start view") {
                g.home();
            }
            ui.add_space(4.0);
            for (icon, which, tip) in [
                (Icon::Time, Drawer::Time, "Time and weather"),
                (Icon::Places, Drawer::Places, "Places"),
                (Icon::Layers, Drawer::Layers, "Layers"),
                (Icon::Settings, Drawer::Settings, "Settings"),
                (Icon::Keys, Drawer::Keys, "Keys (F1)"),
            ] {
                if dock_button(ui, icon, d.drawer == Some(which), tip) {
                    flip(d, which);
                }
            }
            ui.add_space(4.0);
            if dock_button(ui, Icon::Console, d.console, "Console (`)") {
                d.console = !d.console;
                d.focus_input = d.console;
            }
        });
    });
}

fn drawer_panel(ctx: &egui::Context, d: &mut DevUi, g: &mut Game, which: Drawer) {
    let title = match which {
        Drawer::Time => "Time & weather",
        Drawer::Places => "Places",
        Drawer::Layers => "Layers",
        Drawer::Settings => "Settings",
        Drawer::Keys => "Keys",
    };
    let width = if which == Drawer::Keys { 660.0 } else { 360.0 };
    egui::Window::new("drawer")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(Align2::LEFT_CENTER, [70.0, 0.0])
        .min_width(width)
        .max_width(width)
        .frame(frame())
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(16.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if close_x(ui) {
                        d.drawer = None;
                    }
                });
            });
            match which {
                Drawer::Time => time_drawer(ui, g),
                Drawer::Places => places_drawer(ui, d, g),
                Drawer::Layers => layers_drawer(ui, d, g),
                Drawer::Settings => settings_drawer(ui, d, g),
                Drawer::Keys => keys_drawer(ui),
            }
        });
}

fn time_drawer(ui: &mut egui::Ui, g: &mut Game) {
    let Some(tc) = g.tc.as_mut() else {
        ui.label(RichText::new("No time of day in this view").color(DIM));
        return;
    };
    heading(ui, "Time of day");
    ui.horizontal(|ui| {
        ui.label(RichText::new(clock(tc.hour)).monospace().size(22.0).color(AMBER_HI));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for (name, h) in [("Night", 0.0), ("Dusk", 19.5), ("Noon", 12.0), ("Dawn", 6.0)] {
                if button(ui, name, false) {
                    tc.hour = h;
                }
            }
        });
    });
    let mut h = tc.hour;
    let w = ui.available_width();
    if slide(ui, &mut h, w, |v| v / 24.0, |t| (t * 24.0 * 12.0).round() / 12.0) {
        tc.hour = h.rem_euclid(24.0);
    }
    let speeds = [0.0, 1.0, 10.0, 60.0];
    let sel = speeds.iter().position(|&s| (tc.rate - s * BASE_RATE).abs() < 1e-6).unwrap_or(1);
    field(ui, "Clock", |ui| {
        let mut pick = 3 - sel; // right-to-left layout: listed backwards
        if choice(ui, &["x60", "x10", "Normal", "Stop"], &mut pick) {
            tc.rate = speeds[3 - pick] * BASE_RATE;
        }
    });

    heading(ui, "Weather");
    let names: Vec<String> = tc.weather_names().map(String::from).collect();
    egui::Grid::new("weathers").num_columns(2).spacing(vec2(6.0, 6.0)).show(ui, |ui| {
        for (i, n) in names.iter().enumerate() {
            let on = tc.weather == i;
            let b = egui::Button::new(RichText::new(nice_name(n)).color(if on { ON_AMBER } else { TEXT }))
                .fill(if on { AMBER } else { BG2 })
                .corner_radius(rr(4))
                .min_size(vec2(150.0, 26.0));
            if ui.add(b).clicked() {
                tc.weather = i;
            }
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });
    field(ui, "Mixed with", |ui| {
        let current = tc.mix.map_or("Nothing".to_string(), |(w, _)| nice_name(&names[w]));
        egui::ComboBox::from_id_salt("mix").width(110.0).selected_text(current).show_ui(ui, |ui| {
            if ui.selectable_label(tc.mix.is_none(), "Nothing").clicked() {
                tc.mix = None;
            }
            for (i, n) in names.iter().enumerate() {
                if ui.selectable_label(tc.mix.is_some_and(|(w, _)| w == i), nice_name(n)).clicked() {
                    tc.mix = Some((i, tc.mix.map_or(0.3, |m| m.1)));
                }
            }
        });
        if let Some((w, mut amount)) = tc.mix {
            if slide_lin(ui, &mut amount, 0.0, 1.0, 80.0) {
                tc.mix = Some((w, amount));
            }
        }
    });
}

fn places_drawer(ui: &mut egui::Ui, d: &mut DevUi, g: &mut Game) {
    heading(ui, "Save this view");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut d.place_name).hint_text("name").desired_width(240.0));
        if button(ui, "Save", true) {
            let name = std::mem::take(&mut d.place_name);
            save_current(d, g, &name);
        }
    });
    heading(ui, "Saved");
    ui.add(egui::TextEdit::singleline(&mut d.search).hint_text("filter").desired_width(f32::INFINITY));
    let q = d.search.to_ascii_lowercase();
    let shown: Vec<usize> = (0..d.places.len()).filter(|&i| d.places[i].name.to_ascii_lowercase().contains(&q)).collect();
    if shown.is_empty() {
        ui.label(RichText::new(if d.places.is_empty() { "Nothing saved yet" } else { "No match" }).color(DIM));
    }
    let (mut go, mut remove) = (None, None);
    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
        for i in shown {
            let p = &d.places[i];
            // name over position, left-aligned; the row is the button, Delete shows on hover
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
            let painter = ui.painter();
            painter.rect_filled(rect, rr(4), if resp.hovered() { BG3 } else { BG2 });
            painter.text(rect.left_top() + vec2(10.0, 5.0), Align2::LEFT_TOP, &p.name, FontId::proportional(14.0), TEXT);
            let at = format!("{:.0}, {:.0}, {:.0}  ·  facing {:.0}°", p.pos[0], p.pos[1], p.pos[2], p.heading);
            painter.text(rect.left_top() + vec2(10.0, 22.0), Align2::LEFT_TOP, at, FontId::monospace(11.5), DIM);
            let del = egui::Rect::from_center_size(rect.right_center() - vec2(32.0, 0.0), vec2(52.0, 22.0));
            let del_resp = ui.interact(del, ui.id().with(("delete", i)), Sense::click());
            if resp.hovered() || del_resp.hovered() {
                ui.painter().text(del.center(), Align2::CENTER_CENTER, "Delete", FontId::proportional(12.5), if del_resp.hovered() { ERR } else { DIM });
            }
            if del_resp.clicked() {
                remove = Some(i);
            } else if resp.clicked() {
                go = Some(i);
            }
        }
    });
    if let Some(i) = go {
        let p = d.places[i].clone();
        g.teleport(p.pos, Some(p.heading), Some(p.pitch));
    }
    if let Some(i) = remove {
        out(format!("deleted \"{}\"", d.places[i].name));
        d.places.remove(i);
        save_places(&d.places);
    }
}

fn save_current(d: &mut DevUi, g: &Game, name: &str) {
    let Some((pos, heading, pitch)) = g.pose() else { return };
    let name = if name.trim().is_empty() { format!("Spot {}", d.places.len() + 1) } else { name.trim().to_string() };
    out(format!("saved \"{name}\""));
    d.places.push(Place { name, pos, heading, pitch });
    save_places(&d.places);
}

fn layers_drawer(ui: &mut egui::Ui, d: &mut DevUi, g: &mut Game) {
    heading(ui, "On screen");
    field(ui, "Status line", |ui| {
        switch(ui, &mut d.status);
    });
    let mut hud = g.hud.iter().any(|v| *v != Visibility::Hidden);
    field(ui, "Radar and weapon", |ui| {
        if switch(ui, &mut hud) {
            for mut v in &mut g.hud {
                *v = if hud { Visibility::Inherited } else { Visibility::Hidden };
            }
        }
    });
    let mut old = g.hud_text.iter().any(|v| *v != Visibility::Hidden);
    field(ui, "Old text readout", |ui| {
        if switch(ui, &mut old) {
            for mut v in &mut g.hud_text {
                *v = if old { Visibility::Inherited } else { Visibility::Hidden };
            }
        }
    });
    heading(ui, "World");
    if let Some(mut on) = g.roads.as_ref().map(|r| r.on) {
        field(ui, "Traffic", |ui| {
            if switch(ui, &mut on) {
                if let Some(r) = g.roads.as_mut() {
                    r.on = on;
                }
            }
        });
    }
    if let Some(mut on) = g.peds_on.as_ref().map(|r| r.0) {
        field(ui, "Pedestrians", |ui| {
            if switch(ui, &mut on) {
                if let Some(p) = g.peds_on.as_mut() {
                    p.0 = on;
                }
            }
        });
    }
    let mut shadows = g.shadows();
    field(ui, "Sun shadows", |ui| {
        if switch(ui, &mut shadows) {
            g.set_shadows(shadows);
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new("Every other LIBERTY_ switch: env in the console").color(DIM).size(12.0));
}

fn settings_drawer(ui: &mut egui::Ui, d: &mut DevUi, g: &mut Game) {
    heading(ui, "Window");
    let mut full = g.window.single().is_ok_and(|w| !matches!(w.mode, WindowMode::Windowed));
    field(ui, "Fullscreen (F11)", |ui| {
        if switch(ui, &mut full) {
            if let Ok(mut w) = g.window.single_mut() {
                w.mode = if full { WindowMode::BorderlessFullscreen(MonitorSelection::Current) } else { WindowMode::Windowed };
            }
        }
    });
    let mut vsync = g.window.single().is_ok_and(|w| matches!(w.present_mode, PresentMode::AutoVsync | PresentMode::Fifo));
    field(ui, "VSync", |ui| {
        if switch(ui, &mut vsync) {
            if let Ok(mut w) = g.window.single_mut() {
                w.present_mode = if vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync };
            }
        }
    });
    heading(ui, "View");
    let mut fov = g.fov().to_degrees();
    if number(ui, "Field of view", &mut fov, 20.0, 110.0, false, 0, "°") {
        g.set_fov(fov.to_radians());
    }
    if let Some(city) = g.city.as_mut() {
        let mut view = city.view;
        if number(ui, "Draw distance", &mut view, 0.3, 4.0, true, 2, "x") {
            city.view = view;
        }
    }
    let mut shadows = g.shadows();
    field(ui, "Shadows", |ui| {
        if switch(ui, &mut shadows) {
            g.set_shadows(shadows);
        }
    });
    heading(ui, "Camera");
    if let Ok((_, mut fly, _)) = g.cam.single_mut() {
        let mut speed = fly.speed;
        if number(ui, "Fly speed", &mut speed, 0.5, 500.0, true, 1, " m/s") {
            fly.speed = speed;
        }
    }
    ui.add_space(6.0);
    if button(ui, "Defaults", false) {
        g.set_fov(DEFAULT_FOV);
        if let Some(city) = g.city.as_mut() {
            city.view = 1.0;
        }
        if let Ok(mut w) = g.window.single_mut() {
            w.present_mode = PresentMode::AutoVsync;
        }
        g.set_shadows(true);
        d.status = true;
    }
}

fn keys_drawer(ui: &mut egui::Ui) {
    let groups: [(&str, &[(&str, &str)]); 6] = [
        ("Camera", &[("Hold right mouse", "turn the camera"), ("Middle mouse", "lock / unlock mouse-look"), ("W A S D", "move"), ("E / Q", "rise / sink"), ("Shift", "hurry"), ("Mouse wheel", "flying speed"), ("G", "swap flying and walking")]),
        ("Niko", &[("W A S D", "run"), ("Alt", "slow walk"), ("Shift", "sprint"), ("Space", "jump"), ("F", "car: in / out"), ("K / J", "fall limp / take a shove")]),
        ("Guns", &[("1 - 9, X", "pick a gun / the next one"), ("0", "holster"), ("Right mouse or Z", "aim"), ("Left mouse", "shoot"), ("R", "reload")]),
        ("Driving", &[("W / S", "gas / brake"), ("A / D", "steer"), ("Space", "handbrake"), ("C", "new car here"), ("[ / ]", "swap the model"), ("R", "flip upright"), ("H", "horn")]),
        ("World", &[("T", "jump an hour"), ("Y", "cycle weather"), ("+ / -", "draw distance")]),
        ("This panel", &[("`", "console"), ("F1", "this sheet"), ("F2", "hide all of it"), ("F11", "fullscreen"), ("Esc", "close, or quit when all closed")]),
    ];
    ui.columns(2, |cols| {
        for (n, (title, keys)) in groups.iter().enumerate() {
            let ui = &mut cols[n % 2];
            heading(ui, title);
            for (k, what) in keys.iter() {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(138.0, 18.0), Sense::hover());
                    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, *k, FontId::monospace(12.5), AMBER_HI);
                    ui.label(RichText::new(*what).size(13.0));
                });
            }
        }
    });
}

const COMMANDS: &[(&str, &str)] = &[
    ("help", "this list"),
    ("tp x y z [heading] [pitch]", "jump to a GTA position (metres, degrees; heading 0 = north)"),
    ("pos", "print where you are as a tp line (and copy it)"),
    ("home", "back to the start view"),
    ("goto <name>", "jump to a saved place (the first letters do)"),
    ("save [name]", "save this view"),
    ("places", "list saved places"),
    ("hour <0-24>", "time of day"),
    ("rate <x>", "clock speed, 1 = normal, 0 = stopped"),
    ("weather <name|next>", "change the weather"),
    ("fov <degrees>", "vertical field of view"),
    ("view <x>", "draw distance, 0.3 to 4"),
    ("speed <m/s>", "flying speed"),
    ("fly / walk", "camera mode"),
    ("env [NAME[=value]] [-NAME]", "show / set / clear LIBERTY_ switches"),
    ("clear", "wipe this log"),
    ("quit", "leave"),
];

/// Drops down from the top edge, full width.
fn console(ctx: &egui::Context, d: &mut DevUi, g: &mut Game) {
    let screen = ctx.screen_rect();
    let height = (screen.height() * 0.42).max(220.0);
    egui::Area::new(egui::Id::new("console")).fixed_pos(screen.left_top()).order(egui::Order::Foreground).show(ctx, |ui| {
        let r = egui::Frame::new()
            .fill(Color32::from_rgba_premultiplied(9, 10, 13, 244))
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.set_width(screen.width() - 32.0);
                ui.set_height(height);
                egui::ScrollArea::vertical().max_height(height - 36.0).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if let Ok(log) = LOG.lock() {
                        for (kind, line) in log.iter() {
                            let col = match kind {
                                Kind::Info => DIM,
                                Kind::Warn => WARN,
                                Kind::Error => ERR,
                                Kind::Cmd => AMBER,
                                Kind::Out => TEXT,
                            };
                            ui.add(egui::Label::new(RichText::new(line).monospace().size(12.5).color(col)).wrap());
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("liberty>").monospace().color(AMBER));
                    d.input.retain(|c| c != '`');
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut d.input)
                            .frame(false)
                            .desired_width(f32::INFINITY)
                            .font(FontId::monospace(13.0))
                            .hint_text(RichText::new("try help").color(DIM)),
                    );
                    if d.focus_input {
                        resp.request_focus();
                        d.focus_input = false;
                    }
                    let (enter, up, down) =
                        ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
                    if resp.lost_focus() && enter {
                        let line = std::mem::take(&mut d.input);
                        if !line.trim().is_empty() {
                            d.history.push(line.clone());
                            d.hist_at = None;
                            command(ctx, line.trim(), d, g);
                        }
                        resp.request_focus();
                    } else if resp.has_focus() && (up || down) && !d.history.is_empty() {
                        let n = d.history.len();
                        let at = match (d.hist_at, up) {
                            (None, true) => Some(n - 1),
                            (Some(i), true) => Some(i.saturating_sub(1)),
                            (Some(i), false) if i + 1 < n => Some(i + 1),
                            _ => None,
                        };
                        d.hist_at = at;
                        d.input = at.map(|i| d.history[i].clone()).unwrap_or_default();
                    }
                });
            })
            .response
            .rect;
        // amber edge along the bottom
        ui.painter().line_segment([r.left_bottom(), r.right_bottom()], pen(2.0, AMBER));
    });
}

fn quit_prompt(ctx: &egui::Context, d: &mut DevUi, g: &mut Game) {
    let modal = egui::Modal::new(egui::Id::new("quit")).frame(frame()).show(ctx, |ui| {
        ui.set_width(280.0);
        ui.label(RichText::new("Leave Liberty?").size(16.0).strong());
        ui.label(RichText::new("Enter to leave, Esc to stay").color(DIM));
        ui.add_space(8.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if button(ui, "Leave", true) || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                g.exit.write(AppExit::Success);
            }
            if button(ui, "Stay", false) {
                d.quit = false;
            }
        });
    });
    if modal.backdrop_response.clicked() {
        d.quit = false;
    }
}

/// SAFETY: Windows' environment functions are thread-safe (the game is Windows-only), and the
/// switches set here are only read with std::env::var.
fn set_env(name: &str, value: Option<&str>) {
    unsafe {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }
}

fn command(ctx: &egui::Context, line: &str, d: &mut DevUi, g: &mut Game) {
    push(Kind::Cmd, format!("liberty> {line}"));
    let mut words = line.split_whitespace();
    let Some(cmd) = words.next() else { return };
    let args: Vec<&str> = words.collect();
    let rest = args.join(" ");
    let nums: Vec<f32> = args.iter().filter_map(|a| a.trim_end_matches(',').parse().ok()).collect();
    match cmd.to_ascii_lowercase().as_str() {
        "help" | "?" => {
            for (c, what) in COMMANDS {
                out(format!("{c:<30}{what}"));
            }
        }
        "tp" if nums.len() >= 3 => g.teleport([nums[0], nums[1], nums[2]], nums.get(3).copied(), nums.get(4).copied()),
        "tp" => err("tp x y z [heading] [pitch]   (GTA metres; heading 0 = north, 90 = west)"),
        "pos" => {
            if let Some((p, h, pitch)) = g.pose() {
                let line = format!("tp {:.1} {:.1} {:.1} {:.0} {:.0}", p[0], p[1], p[2], h, pitch);
                ctx.copy_text(line.clone());
                out(format!("{line}   (copied)"));
            }
        }
        "home" => g.home(),
        "goto" => {
            let q = rest.to_ascii_lowercase();
            match d.places.iter().find(|p| p.name.to_ascii_lowercase().starts_with(&q)).cloned() {
                Some(p) if !q.is_empty() => {
                    g.teleport(p.pos, Some(p.heading), Some(p.pitch));
                    out(format!("at {}", p.name));
                }
                _ => err(format!("no place starts with \"{rest}\" (places lists them)")),
            }
        }
        "save" => save_current(d, g, &rest),
        "places" => {
            for p in &d.places {
                out(format!("{:<24}{:.0}, {:.0}, {:.0}", p.name, p.pos[0], p.pos[1], p.pos[2]));
            }
        }
        "hour" | "time" => match (g.tc.as_mut(), nums.first()) {
            (Some(tc), Some(&h)) => tc.hour = h.rem_euclid(24.0),
            _ => err("hour <0-24>"),
        },
        "rate" => match (g.tc.as_mut(), nums.first()) {
            (Some(tc), Some(&r)) => tc.rate = r.max(0.0) * BASE_RATE,
            _ => err("rate <x>   (1 = normal, 0 = stopped)"),
        },
        "weather" => {
            let Some(tc) = g.tc.as_mut() else { return };
            let names: Vec<String> = tc.weather_names().map(|n| n.to_ascii_lowercase()).collect();
            let q = rest.to_ascii_lowercase().replace(' ', "_");
            if q == "next" || q.is_empty() {
                tc.weather = (tc.weather + 1) % names.len();
            } else if let Some(i) = names.iter().position(|n| n.starts_with(&q)) {
                tc.weather = i;
            } else {
                err(format!("weathers: {}", names.join(", ")));
                return;
            }
            out(format!("weather: {}", nice_name(tc.weather_name())));
        }
        "fov" => match nums.first() {
            Some(&f) => g.set_fov(f.to_radians()),
            None => out(format!("fov {:.1}", g.fov().to_degrees())),
        },
        "view" => match (g.city.as_mut(), nums.first()) {
            (Some(c), Some(&v)) => c.view = v.clamp(0.3, 4.0),
            _ => err("view <0.3 - 4>"),
        },
        "speed" => match (g.cam.single_mut(), nums.first()) {
            (Ok((_, mut fly, _)), Some(&s)) => fly.speed = s.clamp(0.5, 500.0),
            _ => err("speed <m/s>"),
        },
        "fly" => g.set_walking(false),
        "walk" => g.set_walking(true),
        "env" => env_command(&args),
        "clear" | "cls" => {
            if let Ok(mut log) = LOG.lock() {
                log.clear();
            }
        }
        "quit" | "exit" => d.quit = true,
        other => err(format!("no command \"{other}\" (help lists them)")),
    }
}

fn env_command(args: &[&str]) {
    let full = |n: &str| {
        let n = n.to_ascii_uppercase();
        if n.starts_with("LIBERTY_") { n } else { format!("LIBERTY_{n}") }
    };
    if args.is_empty() {
        let mut set: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("LIBERTY_")).collect();
        set.sort();
        if set.is_empty() {
            out("no LIBERTY_ switches set");
        }
        for (k, v) in set {
            out(format!("{k}={v}"));
        }
        return;
    }
    for a in args {
        if let Some(name) = a.strip_prefix('-') {
            set_env(&full(name), None);
            out(format!("{} cleared", full(name)));
        } else if let Some((name, value)) = a.split_once('=') {
            set_env(&full(name), Some(value));
            out(format!("{}={value}", full(name)));
        } else {
            let n = full(a);
            out(match std::env::var(&n) {
                Ok(v) => format!("{n}={v}"),
                Err(_) => format!("{n} is not set"),
            });
        }
    }
    out("(switches read every frame act now; ones read at start need a restart)");
}
