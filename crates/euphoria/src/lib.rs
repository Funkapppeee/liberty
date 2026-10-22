//! Clean-room Euphoria behaviours. Every rule here comes from a spec in `specs/euphoria/`
//! (see CLEANROOM.md); numbers are checked against the Reader's test vectors.
//!
//! Frame: GTA's own Z-up world, no axis conversion inside a behaviour.

pub mod armik;
pub mod balancer;
pub mod bodybalance;
pub mod footplace;
pub mod legs;
pub mod pelvis;
pub mod reach;
pub mod brace;
pub mod catchfall;
pub mod fall;
pub mod headlook;
pub mod math;
pub mod rng;
pub mod torso;
pub mod wristik;
