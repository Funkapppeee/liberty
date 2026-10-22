# headLook (NmRsCBUHeadLook): spec draft 0 (reader side, 2 Oct 2026)

Read from the structure of IV's routines (activate 0x0084d6b0, tick 0x0084d940; decompiles in
`out/ghidra/decomp/ART_Rockstar_NmRsCBUHeadLook/`). Not yet run in the harness: every formula below
is to be confirmed against IV's own routine on test inputs before it counts as exact.

## What it does
Turns the neck and head toward a point. It works out three angles between where the head points
and where the target is (a lean forward/back, a lean sideways, a twist), splits each angle between
the neck joint and the head joint in proportion to how far each joint can bend that way, clamps,
and writes them as the two joints' wanted angles. When the target is too far round to reach with
the neck alone, it also twists the two upper spine joints toward it.

## Joints it drives (character's effector table at character +0x280)
- +0x24: neck (wanted angles written at effector +0x164 lean1, +0x168 lean2, +0x16C twist)
- +0x2C: head (same slots)
- +0x1C and +0x14: the upper spine joints (only their twist, +0x16C, when the target is behind)
- An effector whose flag byte +0xE8 has bit 0x04 set is left alone (something else owns it).

## Settings (behaviour object)
- +0x1C..+0x24: target position; +0x28..+0x30: target velocity; +0x3C: object instance (-1 = the
  position is in world space; otherwise it's relative to that object and moved with it).
- +0x38: read at the start of each tick (purpose not settled).
- +0x48: half-angle of the "in front" cone, set to 1.0472 (60 degrees) on activate.
- +0x98 flags: 0x01 active; 0x08 "target in front, looking"; 0x02 / 0x04 more conditions (always
  look? keep the head up? to be named against NaturalMotion's settings list).

## On activate (once)
- Copies the neck's and head's joint limits (effector +0x190..+0x1A4: lean1 min/max, lean2
  min/max, twist min/max) into +0x4C..+0x60 (neck) and +0x6C..+0x80 (head). Sums of the lean1
  maxima go to +0x84, of the lean2 to +0x88.
- +0x8C = 0.5 x (neck + head lean1 range); +0x90 = 0.25 x (neck + head lean2 range): the same pair
  is recomputed each tick.
- +0x94 = the distance between two body points (head and neck, presumably): the eyes' offset.

## Each tick
1. Bail out unless the character has a body and the behaviour is active.
2. The target in world space (moved with its object if one is set), led by its velocity times a
   lead factor (+0x90 / +0x8C).
3. The head's frame: its forward axis is rotated by a fixed -0.2382 (about 13.6 degrees) about a
   head axis, so "looking straight" points slightly down. The eye point is the head plus +0x94
   along that forward axis.
4. Is the target inside the front cone (dot of forward with the direction to the target against a
   cone that widens from +0x48 toward 90 degrees with how far round it is)? Sets / clears flag 0x08.
   If neither 0x08 nor 0x02 is set, nothing more is done (the head is left to whatever drives it).
5. If the target is more than 1.2 x (+0x88) round, the spine helps: the excess angle (halved, sign
   from which side) twists the two upper spine joints: 0.65 of it on the joint at +0x1C, 0.35 on
   +0x14, each clamped to 1.35 x its own twist limits. The head's target direction is rotated back by
   the same amount before the rest.
6. The three angles (twist, lean1, lean2) between the head's axes and the target direction, each
   from an arc cosine with a sign from a cross product. If the up axis is nearly vertical
   (|dot| > 0.9) or flag 0x04 is set, the twist is measured against the world up instead.
7. Each angle is split: positive angles by the joints' positive limits, negative by the negative
   ones: neck share = neck limit / (neck limit + head limit), head share likewise; each part is
   clamped to -9..9 rad (effectively unclamped) and written to that joint's wanted angle.

## Constants
0.5, 0.25 (ranges), -0.238196 (head forward tilt, rad), 2.0, 1.0, 1.570796, 1.2 (spine help
threshold), -0.5, 0.9, 1.35 (spine twist limit factor), 0.65 / 0.35 (spine split), -9 / 9 (clamp).

## Open
- What drives the neck and head stiffness while looking (not set here; activate or the caller).
- Flags 0x02 and 0x04 by name; +0x38.
- The two body points behind +0x94.
- Harness vectors, then the port (crates/euphoria::headlook), then the shot's schedule calling it.

## Harness findings (2 Oct, late)
- **IV's routine now runs** on the Fred.art rig: `re/vec_headlook.py` (Rig(head=True) adds the neck and head to the spine limb: +0x24 neck joint, +0x28 neck part, +0x2C head joint, +0x30 head part). Writes out/vectors/headlook.json: 240 target directions over the sphere, 3 m from the head; per case the neck / head wanted lean1, lean2, twist and both upper spine twists.
- **Corrections to draft 0:**
  - +0x34 = stiffness, +0x38 = damping ratio. Every tick sets the "un" joints (mask string at 0xfc5698: neck + head) to strength = stiffness^2 (effector vfn +0x4C) and damping = 2 x stiffness x ratio (vfn +0x50), through the set-by-mask helper 0x82f860 (ecx character, edx mask, xmm2 value). Left at 0 they give strength 0 and the tick divides 0 by 0.
  - +0x8C = 0.5 x (neck + head strength) and +0x90 = 0.25 x (neck + head damping) (effector vfns +0x5C / +0x60 read them back), not angle ranges. Their ratio is the target velocity lead.
  - Flag 0x08 = "target in headLook's front region"; the front is the head part's -x axis (in the rig's rest pose that points along world -X while the body faces +Y). 142 of 240 directions are in it.
- **Encrypted helper 0x4727d0** (rotate a vector by a quaternion; thiscall ecx = quaternion x y z w, [esp+4] in, [esp+8] out, pops 8) is stood in (`Emu.install_quat_rotate`); **handedness settled: q v q\*** (the standard rotation). In all 38 spine-help cases of the sweep it leaves the neck and head less to turn (1.361 rad, exactly their two twist limits), while q\* v q leaves more (1.46-2.38 rad); the rotation exists to take the spine's share off the head's target, so only q v q\* does its job.
- Vectors use stiffness 14.5 / ratio 1.5, the values real shot victims' necks ran at in the 1 Oct session.
- Next: vary the head's pose (the rig only has one), then port and replay every case.

## Fitted against IV (re/fit_headlook.py, 2 Oct night)
Our own re-implementation from this spec matches IV's routine on **238 of 240** sphere directions within 1e-4 rad (worst 1.7e-4, float rounding near-degenerate cases); the front-region decision agrees on 240/240. Settled on the way:
- The reference frame is **spine3's** part frame (not the head's): rows a and c turned by -0.238196 rad about row b give A (the axis the twist is measured about) and C (the facing axis). The eye point is spine3's position + A x eye offset (+0x94 = spine3-to-head distance, 0.212 m on Fred).
- The working direction is **eye minus target** (from the target back to the eyes).
- Twist about A: the target direction flattened onto the plane across A, against C. Beyond 1.2 x (neck + head twist max) the excess goes to the spine: 0.65 to spine3, 0.35 to spine2, added to their current wanted twist, each clamped to 1.35 x its own twist limits; the direction is turned back by the excess (q v q*) before the head angles.
- Up-tilt removed by turning K (the direction's "up", from (A x V) x ... ) onto A; then twist = acos(V'.C) with sign from V'.b, lean1 = acos(M1.A) with sign from M1.C, lean2 = acos(M2.A) with sign from M2.b; split between neck and head by their limits on that side.
Not yet exercised by the sweep (one rest pose, nothing moving): the world-up branch (|A.up|^2 > 0.25 or flag 0x04), the near-vertical fallback, the velocity lead and body spin, an object-relative target, locked effectors, non-zero current spine twist. Next: widen the sweep over these, break the fit on purpose, then port to crates/euphoria.

## Ported (crates/euphoria::headlook, 3 Oct)
`headlook::step(&Input) -> (in_front, Option<Angles>)`. Test `crates/euphoria/tests/headlook_vectors.rs` replays out/vectors/headlook_cases.json (400 random cases from `re/fit_headlook.py 400 random dump`: random spine3 and head poses, target distance 0.5-6 m and velocity, body velocity and spin, starting spine twists, flags 0x02 and 0x04): all 400 within 2e-4 rad, front decision 400/400. Break checks (each must fail): head tilt -0.238 -> -0.2 (278 fail), spine3 share 0.65 -> 0.6 (31), lean2 sign (275), spine-help threshold 1.2 -> 1.1 (121); on the Python fit also the clamp, world-up flag, near-vertical fallback, velocity lead, body spin and look-anyway flag were each caught.
Not covered: an object-relative target (+0x3C != -1) and locked effectors (effector +0xE8 bit 0x04), both just skip work in IV.
