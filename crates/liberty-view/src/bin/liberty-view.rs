//! The liberty-view exe: the viewer with nothing added. Everything lives in the library (lib.rs),
//! so another crate can run the same game with its own Bevy plugins/systems through
//! `liberty_view::run_with`.

fn main() -> anyhow::Result<()> {
    liberty_view::run()
}
