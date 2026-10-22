//! Middle mouse locks mouse-look, like the real game: the cursor is grabbed and hidden and the
//! camera follows the mouse without holding the right button. Holding right still looks as before.
//! The lock lets go when a dev panel opens (egui needs the cursor), on Esc or F2, and when the
//! window loses focus; middle mouse again locks it. It is one flag for every camera (fly, walk,
//! drive), so it survives switching between them.

use std::sync::atomic::{AtomicBool, Ordering};

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow, WindowFocused, WindowMode};

static LOCKED: AtomicBool = AtomicBool::new(false);

/// Mouse motion turns the camera: the lock is on, or the right button is held.
pub fn looking(buttons: &ButtonInput<MouseButton>) -> bool {
    LOCKED.load(Ordering::Relaxed) || buttons.pressed(MouseButton::Right)
}

pub fn plugin(app: &mut App) {
    app.add_systems(PreUpdate, look_lock.after(bevy::input::InputSystem));
}

fn look_lock(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut focus: EventReader<WindowFocused>,
    devui: Option<Res<crate::devui::DevUi>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    // scripted test runs (LIBERTY_NO_INPUT) never grab the cursor of whoever is using the PC
    if std::env::var_os("LIBERTY_NO_INPUT").is_some() {
        return;
    }
    let was = LOCKED.load(Ordering::Relaxed);
    let mut on = was;
    if buttons.just_pressed(MouseButton::Middle) {
        on = !on;
    }
    let lost_focus = focus.read().fold(false, |lost, e| lost || !e.focused);
    let panel = devui.is_some_and(|d| d.wants_cursor());
    if lost_focus || panel || keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::F2) {
        on = false;
    }
    LOCKED.store(on, Ordering::Relaxed);
    let Ok(mut w) = windows.single_mut() else { return };
    if on != was {
        // Windows can't lock the cursor in place; Bevy falls back to keeping it inside the window
        w.cursor_options.grab_mode = if on { CursorGrabMode::Locked } else { CursorGrabMode::None };
        // unlocked: shown in a window, hidden in full screen (as F11 leaves it)
        w.cursor_options.visible = !on && matches!(w.mode, WindowMode::Windowed);
    }
    if on {
        // keep the hidden cursor in the middle so it never rests on the window's edge
        let mid = Vec2::new(w.width(), w.height()) / 2.0;
        w.set_cursor_position(Some(mid));
    }
}
