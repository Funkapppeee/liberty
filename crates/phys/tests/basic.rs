use glam::{DMat3, DQuat, DVec3};
use phys::artic::{Body, Drive, JointDef, JointKind, LinkDef, Pose};
use phys::contact::{generate, Collider, Shape, Tri};

const G: DVec3 = DVec3::new(0.0, -9.81, 0.0);

fn capsule_inertia(m: f64, r: f64, half: f64) -> DMat3 {
    let l = 2.0 * half;
    let ixx = m * (3.0 * r * r + l * l) / 12.0;
    DMat3::from_diagonal(DVec3::new(ixx, ixx, 0.5 * m * r * r))
}

/// A three-link "arm": root capsule, a ball joint, a hinge. Capsules along local Z.
fn arm() -> Body {
    let root = LinkDef { mass: 5.0, com: DVec3::ZERO, inertia: capsule_inertia(5.0, 0.1, 0.2), joint: None };
    let ball = LinkDef {
        mass: 2.0,
        com: DVec3::new(0.0, 0.0, 0.15),
        inertia: capsule_inertia(2.0, 0.05, 0.15),
        joint: Some(JointDef {
            parent: 0,
            frame_parent: Pose::new(DVec3::new(0.0, 0.0, 0.25), DQuat::IDENTITY),
            frame_child: Pose::IDENTITY,
            kind: JointKind::Ball,
            limits: [Some([-1.0, 1.0]), Some([-1.5, 1.5]), Some([-1.5, 1.5])],
        }),
    };
    let hinge = LinkDef {
        mass: 1.5,
        com: DVec3::new(0.0, 0.0, 0.15),
        inertia: capsule_inertia(1.5, 0.05, 0.15),
        joint: Some(JointDef {
            parent: 1,
            frame_parent: Pose::new(DVec3::new(0.0, 0.0, 0.3), DQuat::IDENTITY),
            frame_child: Pose::IDENTITY,
            kind: JointKind::Hinge(0),
            limits: [Some([0.0, 2.6]), None, None],
        }),
    };
    Body::new(vec![root, ball, hinge], G)
}

fn anchors_joined(b: &Body) -> f64 {
    (1..b.links.len()).map(|i| (b.joint_frame_parent(i).pos - b.joint_frame_child(i).pos).length()).fold(0.0, f64::max)
}

#[test]
fn free_fall_matches_gravity() {
    let mut b = arm();
    b.root.pos = DVec3::new(0.0, 10.0, 0.0);
    b.update_kinematics();
    let h = 1.0 / 240.0;
    for _ in 0..240 {
        b.step(h, &[]);
    }
    // total COM velocity after 1 s ≈ g (light damping 0.05/s allowed)
    let m = b.mass();
    let v: DVec3 = (0..3).map(|i| b.vcom[i] * b.links[i].mass).sum::<DVec3>() / m;
    assert!((v.y + 9.81).abs() < 0.35, "COM velocity after 1 s: {v:?}");
}

#[test]
fn joints_never_separate_while_tumbling() {
    let mut b = arm();
    b.root.pos = DVec3::new(0.0, 5.0, 0.0);
    b.root_angvel = DVec3::new(3.0, 5.0, -2.0);
    b.u[1] = DVec3::new(4.0, -3.0, 2.0);
    b.update_kinematics();
    for _ in 0..600 {
        b.step(1.0 / 240.0, &[]);
        assert!(b.pose.iter().all(|p| p.pos.is_finite()));
    }
    assert!(anchors_joined(&b) < 1e-9, "anchor gap {}", anchors_joined(&b));
}

#[test]
fn drive_reaches_target_against_gravity() {
    let mut b = arm();
    b.root.pos = DVec3::new(0.0, 5.0, 0.0);
    b.update_kinematics();
    // pin the root by resetting it each step; drive the hinge to 1.2 rad
    b.drives[2] = Some(Drive { target: [1.2, 0.0, 0.0], stiffness: 144.0, damping: 12.0 });
    b.drives[1] = Some(Drive { target: [0.0, 0.0, 0.0], stiffness: 144.0, damping: 12.0 });
    for _ in 0..240 {
        b.root.pos = DVec3::new(0.0, 5.0, 0.0);
        b.root_vel = DVec3::ZERO;
        b.root_angvel = DVec3::ZERO;
        b.root.rot = DQuat::IDENTITY;
        b.step(1.0 / 240.0, &[]);
    }
    let a = b.angles(2)[0];
    assert!((a - 1.2).abs() < 0.15, "hinge angle after 1 s: {a}");
}

#[test]
fn rests_on_floor_without_sinking() {
    let mut b = arm();
    b.root.pos = DVec3::new(0.0, 0.3, 0.0);
    b.root.rot = DQuat::from_rotation_y(std::f64::consts::FRAC_PI_2); // capsules lying along the ground (local Z -> X)
    b.update_kinematics();
    let floor = vec![
        Tri::new(DVec3::new(-10.0, 0.0, -10.0), DVec3::new(10.0, 0.0, -10.0), DVec3::new(10.0, 0.0, 10.0)),
        Tri::new(DVec3::new(-10.0, 0.0, -10.0), DVec3::new(10.0, 0.0, 10.0), DVec3::new(-10.0, 0.0, 10.0)),
    ];
    let cols = vec![
        Collider { link: 0, local: Pose::IDENTITY, shape: Shape::Capsule { radius: 0.1, half_len: 0.2 }, friction: 0.9 },
        Collider { link: 1, local: Pose::new(DVec3::new(0.0, 0.0, 0.15), DQuat::IDENTITY), shape: Shape::Capsule { radius: 0.05, half_len: 0.15 }, friction: 0.9 },
        Collider { link: 2, local: Pose::new(DVec3::new(0.0, 0.0, 0.15), DQuat::IDENTITY), shape: Shape::Capsule { radius: 0.05, half_len: 0.15 }, friction: 0.9 },
    ];
    for _ in 0..720 {
        let touches = generate(&b, &cols, &floor, &[], DVec3::Y);
        let contacts: Vec<_> = touches.iter().map(|t| t.contact).collect();
        b.step(1.0 / 240.0, &contacts);
    }
    let low = (0..3).map(|i| b.com[i].y).fold(f64::MAX, f64::min);
    let speed = (0..3).map(|i| b.vcom[i].length()).fold(0.0, f64::max);
    assert!(low > 0.02, "sank: lowest COM {low}");
    assert!(speed < 0.05, "still moving at {speed}");
    assert!(anchors_joined(&b) < 1e-9);
}
