//! Liberty's own physics, written to replace Rapier (and, step by step, to match RAGE's own
//! `phArticulatedBody` / `phJoint3Dof` / `phJoint1Dof`).
//!
//! - `artic`: an articulated body in joint coordinates (reduced coordinates): a floating root link
//!   and a tree of links joined by ball (3-DOF) or hinge (1-DOF) joints. Joints can't come apart.
//!   Muscles (joint PD drives) and joint limits are integrated implicitly, so stiff muscles are stable.
//! - `contact`: sphere/capsule/box colliders against triangles and moving boxes, and the contact
//!   impulse solve on the articulated body.
//!
//! Everything is f64 inside; the axes are whatever the caller uses (Liberty passes Bevy's Y-up).

pub mod artic;
pub mod contact;
pub mod linalg;
pub use glam;
