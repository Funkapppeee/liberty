# Dynamic balancer (IV 1.2.0.59) — draft 1

Behaviour and numbers only, measured by running IV's code in the harness (`re/vec_balfail.py`) and
checked with `re/fit_balfail.py`. Rust: `crates/euphoria/src/balancer.rs`, test `tests/balfail_vectors.rs`.

## Balance check (matched 400/400 on the fail decision, 95/95 on the stability value)
Inputs:
- u is the up axis.
- C is the whole-body COM.
- L and R are the foot part positions.

The horizontal distance between two points is hdist(a, b) = |(a − b) − u·((a − b)·u)|.

Steps:
- m = min(hdist(C, L), hdist(C, R), hdist(C, (L + R)/2))
- h = u·C − min(u·L, u·R)
- **Failed** when any of these holds:
  - `0.6·h < m`. The 0.6 is balanceAbortThreshold.
  - `h < 0.5`, when the give-up flag is on (it is on by default).
  - The balancer's state is −1.

  A failure sets the balancer's failed flag and sends the "balance" feedback. The stability value is not updated.
- **Stability**, when not failed:
  - Look 0.3 s ahead: C' = C + 0.3·v_com, and each foot' = foot + 0.3·v_foot. The midpoint still uses the un-offset feet.
  - m' = the same minimum, computed with C' and the feet'.
  - s = m' / (0.18·h).
  - If s ≤ prev, it falls slowly: s = prev + 0.25·(s − prev). It rises at once.
  - When s < 1, h > 0.7 and the state is > 2, the value is also sent as an event.

## Foot placement (matched on all 1200 harness cases; Python model `re/fit_footplace.py`, Rust `src/footplace.rs`)
Inputs:
- **Tuning:** the up axis; forward; each foot's reference point and ground normal; dt; min separation (0 by default); side offset (0.08); lead time (0.2); support margin (0); gap limit (0.1); reach scale (1).
- **Body:** COM, COM velocity and its horizontal part; the body's side and front axes; the pelvis frame; each foot's frame, velocity and spin; each hip's frame, half-ranges, centres and current angles; foot contacts.

Each update:
1. **Foot targets.** Each foot's target is its position moved along up to its reference height. When the feet aren't both down and are closer than the min separation, push them apart equally. The midpoint is set halfway between them.
2. **Support check.** Lead the COM by `lead time × horizontal velocity`, at the midpoint's height. The quad has two corners per foot: the foot position + a·(∓0.05) ± c·0.15, where the left foot uses −0.05 and the right uses +0.05. If the led point is further than the margin inside the quad, set SUPPORTED. Clear it again when the feet are more than min separation + 0.2 apart horizontally.
3. **Catch targets.** For each foot, n = the horizontal unit vector from the *other* foot to the led point. The target is the led point ± side offset·(front·(side·n)(front·n) + side): the left foot uses −, the right +. `need[i]` = the horizontal distance from foot i to its target.
4. **Deciding a step** (only when not already stepping), with l and r = the contact flags:
   - Candidates:
     - Left may step when (r down: left needs more, or l up) / (r up: l up and right needs more).
     - Right is symmetric.
     - "Needs more" also requires not SUPPORTED.
   - Every candidate step uses up one of `steps_left` (negative means uncounted). At 0, no step is taken.
   - The step records whether the stepping foot started unsupported while the other was down (SWING_FREE), and whether the other foot had further to go (OTHER_FURTHER). It also records its start position.
   - When not stepping, the down flags copy the contacts.
5. **While stepping**, where i is the stepping foot:
   - **Target and approach.**
     - Clamp the catch target into the hip reach ellipse. It is centred at the pelvis + 0.4·(up·b)·(c_h × up) + 0.4·(up·c)·c_h, where c_h is the horizontal unit of c. Its radii are 1.5·halfA·reach (along c_h) and 2.5·halfB·reach (across).
     - Track the foot's back point (pos − 0.1·c) against the previous target. `closing` = (old remaining − new remaining)/dt. It becomes −1 when the foot has passed the target, or when the predicted foot axis (c + 0.07·(ω × c)) points below the ground normal.
   - **Lift.**
     - Hip swing angles are taken to the target at the foot's height and to the start. Each is aim angle = acos of the axial component, spread over the two lean axes, plus the centres; the right side is mirrored.
     - The swing room runs from the mid-swing point, along the swing, out to the limit ellipse (1.5 × half-ranges), measured past the current angles.
     - lift = min(room, min(|tilt|, 1)·0.1 + clamp(0.5·(0.1·min(v_rel, 0) + remaining along the step), ±gap limit)).
     - If the foot started down, lift += 0.07 and the rest point is pushed forward by the start's forward excess.
   - **Moving the target.**
     - The raised target height = up·rest + lift·1.5/(n·up + 0.5).
     - Move it horizontally by its offset from the current placement, at most 0.1 m, then clamp it to the reach again. That gives the new placement.
     - The midpoint's height = the average of both placements. Horizontally it is the COM + 0.15·horizontal velocity.
   - **Landing.** The step ends when the stepping foot is down and the other is not. It also ends when the foot is down, `closing` < 0, and either progress (remaining/total horizontal) < 0.5 or both SWING_FREE and OTHER_FURTHER are set. On landing the foot's down flag is set and a new step can be decided the same update.

## Pelvis control (matched on all 1200 harness cases; `re/fit_pelvis.py`, Rust `src/pelvis.rs`)
State:
- the pelvis offset from the COM (horizontal);
- an optional override velocity;
- an anchor point;
- drift, front and side integrators;
- a twist target per hip;
- side and front lean outputs;
- stance flags per leg.

Each update:
- **Heading** of a velocity v about the pelvis: atan2(−v·c, −(v·b)·sign), turned a quarter.
  - Normally it is folded toward 0 from either side.
  - When the override velocity is used, it is left unfolded instead: angles past ±0.8π read as 0. v is first turned back by 0.2 s of body spin, and the result is weighted by min(|v|, 1)·0.025.
- **Twist.** v = the override velocity if set, else the COM's horizontal velocity.
  - **Single stance:** the stance hip's twist follows clamp(heading, ±0.6) at rate 0.2·min(2|v|, 1) per update. The other hip takes half of it; for the right hip the heading is mirrored.
  - **Both or neither in stance, with the override:** r = clamp(weighted heading + half the twist difference, ±0.6). The twists become (r, −r).
  - **Otherwise:** each twist relaxes toward range − centre (left) or −range − centre (right) at dt/2 per update.
- **Lean.** Lead = lead time × 476. The body's front and side axes are taken horizontally, with the side axis negated.
  - The COM's offset from the anchor and its velocity are measured along both axes. The front offset is clamped to [−0.075, 0.15].
  - **Both feet in stance:** the front integrator += dt·front offset, clamped to base ± 0.1. The side integrator += dt·side offset, clamped to ±0.1.
  - Side lean = (104·side integrator + 238·side offset + lead/2·side velocity)/288.
  - Front lean = −(208·front integrator + 476·front offset + lead·front velocity)/144 in double stance. Otherwise it is 0, and drift −= clamp(−0.2·front velocity, 0, 0.5).
  - **Tipping:** when −(n_L·c_L) − (n_R·c_R) > 0.05, where c is each foot's axis and n its ground normal, the front lean is scaled by 1 − min(3·excess, 1).

## Leg solve (matched on all 800 harness cases, with its leg IK on all 1600 calls; `re/fit_legs.py`, `re/fit_legik.py`, Rust `src/legs.rs`)
Inputs:
- The placements and down flags from foot placement.
- From pelvis control: the offset, drift, twists, height target and leans.
- **Body:** each hip's frame, strength/damping, twist limit, half-ranges, centres, desired and current angles; each knee's two frames and desired/current angles; ankle strength/damping; calf, foot and pelvis frames with velocities; each leg's limb frame, whose translation is the thigh.

Outputs per leg: hip (lean1, lean2, twist), knee, and ankle (lean1, lean2). Also the spine's side lean and the pelvis height target, which is fed back next update.

1. **Height target.** For each foot, a = min(((horizontal distance to the midpoint − spread)·1.3)², 0.5625·h²), where h is the hip height. That foot's low point = up·foot − (h − √(h² − a)).
   - height = h·straightness − (up·mid − the lower low point), + 0.03 unless both feet are down.
   - A down left foot is raised by lean_side·spread; a down right foot is lowered by the same amount.
   - The spine lean starts as clamp(lean_side, ±0.2).
2. **Desired pelvis frame** (when a foot is down):
   - Base axes are (up, w, up × w), with w = unit(pelvis c × up).
   - Mix them by the rotation (R_z(−lean_side)·R_y(−drift))ᵀ, then turn by −0.1 s of pelvis spin.
   - Origin = mid + up·height target + pelvis offset.
   - Each down foot is clamped into its hip's reach ellipse (as in foot placement) about this frame.
   - Then the down foot's target is taken into the desired frame's own coordinates and put back out through the current pelvis frame (so a pelvis that sits lower than wanted pushes the foot target down, which straightens the leg); the same for its ground normal. **Corrected 2 Oct 2026:** draft 1 had these two the other way round, from an emulator stand-in with a bug (see "Checked against the real game").
3. **Leg IK** per leg, with twist = that hip's twist, mirror ±1, knee sign −1 and blend 1.
   - **Planted foot:** no lead.
   - **Swing foot:**
     - Lead the target by strength/(damping + 0.1)·step_predict of pelvis motion: the spin moves it, then the pelvis velocity takes it back, with the COM velocity subtracted. Keep it at least 0.1 m out along the hip→target line.
     - Pull the target toward the foot by the straightening s = clamp(4·up·(target − predicted foot) + 0.1, 0.15, 0.6). The predicted foot is the foot + 0.05·(**left** foot velocity − ground velocity) for both legs. If 1 − 0.1/m is less than s, s becomes the mean of the two. m = min(|target − foot|, 5·strength/damping) + 1e-4.
     - The result is blended with the current angles by 1/(1.0001 − s).
   - **IK:**
     - The shin is taken in the limb frame; its current bend is asin(hinge·(shin × thigh)/(|shin||thigh|)).
     - The target is clipped to thigh + shin. knee = −(π − interior angle) + current bend.
     - The hip rotation takes the bent foot onto the target, times the twist about the hip→foot line. It is read as (twist, swing1, swing2).
     - A twist past the limit redoes the swing with the excess taken off. **The twist output keeps the unclamped value.**
     - A swing outside the lean ellipse adds clamp(−(1 − 1/√e)·|swing|, ±0.5) to the knee.
     - hip = centres + (swing1, mirror·swing2, mirror·twist).
4. **Ankles.**
   - **Swing:**
     - (calf c·n − (calf spin·calf a)·strength/damping)·0.2 + 0.8·(unit(foot a × up)·n).
     - −0.2 when the foot moves backward along the body front.
     - + acos(clamp(straightness, 1e-4, 0.999)).
   - **Planted:** asin(knee hinge·(unit(predicted knee − target) × n)). The predicted knee = the hip frame turned by the new hip angles, pushed out by the thigh length.
   - lean2 = hip twist/2.
5. **Leans and spill.**
   - Down feet get lean_front ± spine lean on ankle lean1.
   - A hip's lean2 overflow past its lean ellipse, (√e − 1)/√e·(lean2 − centre), moves to the other hip. It is taken off this hip only if that foot is down, and taken off the spine lean.

## Applying the targets (not ported yet; facts for the port)
Each update, after the leg solve, the targets go onto the effectors. A joint that is under another controller keeps its own values.
- **Targets:** hip lean1/lean2/twist, knee, ankle. The ankle's lean1 is clamped to ±9.5. The spine's lean2 gets the spine lean.
- **Height target:** fed back as the pelvis height input.
- **Ankle muscle stiffness:** 10 when that foot touches the ground. Otherwise clamp(1 + 4·(60·dt − 1), 1, 5).
- **Ankle oppose-gravity:** 2·(ground normal·foot b axis)².
- **Knee strength:** (legStiffness·scale)², with IV defaults 12 × 1 → 144. For a planted knee (with the balancer's "soften" flag, on by default) it is first scaled by clamp(1 − 1.218·(−knee angle − 1.5), 0.2, 1), so a knee bent past 1.5 rad softens.
- **Knee muscle stiffness:** 1.5 planted, 2.0 swinging.
- **Stance ids:** the character is told which leg is swinging: its foot id, −1 for the planted one, −2 for none. The contacts use this.
(Field meanings are read from the effector setters: 3-DOF +0x184 / +0x17c next to strength/damping at +0x188/+0x18c, and 1-DOF +0x12c / +0x128. The names "muscle stiffness" and "oppose gravity" are the most likely reading. They get confirmed when the drive is matched.)

## Checked against the real game (2 Oct 2026)
- A real balancing pedestrian's balancer was read out of the session's memory recording
  (`re/bal_snapshot.py`, `re/bal_real_case.py`): every block the leg solve reads, and the hip /
  knee / ankle targets the game had stored.
- IV's own leg solve run in the emulator on those blocks now gives the stored targets to 4
  decimals (`re/vec_legs_real.py`), and so does the Rust port (test
  `leg_solve_matches_the_real_game`, worst difference 4.4e-4; the stored values are one update old).
- This caught a bug in the emulator's stand-in for an encrypted matrix-product helper: it always
  multiplied in the wrong order. All balancer vectors were regenerated with the fix; foot
  placement, pelvis control and the other ports still pass unchanged, the leg solve needed the
  one correction above.
- How IV describes the body to the balancer, read from the same moment: a part's frame is the
  part matrix's own axes (pelvis: up, left, back; foot: right, up, back), IV's body-frame "front"
  row points behind the character, hip frames are the joints' parent-side frames, a knee's two
  frames have b down the bone and c along the hinge toward the character's right, and the limb
  frame is the knee's thigh-side frame in the hip's child-side frame (a constant of the rig; ours
  matches the recorded one).
- Real tuning seen (pistol-shot pedestrian, standing): hip height 0.8873, hip spread 0.1289,
  straightness 0.9518, lead time 0.2, step predict 1.0, gap limit 0.1, reach 1.0, and at the
  slots this spec calls min separation / twist centre / twist range / side offset: 0.3568 /
  0.1895 / 0.0156 / 0.187 (so those four are not the 0 / 0.08 defaults assumed in draft 1).
  Joint muscle slots: hips, knees, ankles 24 and 144 (2 x 12 x 1 and 12 squared), spine 27 and
  182.25 (stiffness 13.5).

## The joint muscle model (structure and numbers; not ported)
Each joint (effector) carries a strength k, a damping c and a "muscle stiffness" m. Every update
it hands the physics joint:
- strength k and damping c for all three axes (c is first raised to a minimum passed in);
- the target angles, relative to the limit centres, with the two leans kept inside a cone of
  radius 20 / (k + tiny) + 1 (in half-range units) and the twist inside the same factor of its range;
- a joint stiffness s between 0 and 1: `x = (8 c + k) x 0.5 x m x dt`, `s = x / (1 + x)`, at least
  `6 x dt` (0.1 at 60 Hz), at most 1;
- torque limits: the joint's two caps (300 and 300 in Fred.art) times `1 / (1 - s)`.
So muscle stiffness is not a torque gain by itself: it sets how close to rigid the physics joint
is made (1 / (1 - s) = 1 + x), and it raises the torque cap by the same factor.

Real values (the shot pedestrian of the session, while standing, per-frame joint records):
legs' strength about 100 (it varies 91 to 122 from frame to frame), damping about 10 (so
stiffness 10 with damping ratio 0.5), muscle stiffness 0.375 on hips and knees and 7.5 on the
ankles (5 before the hit; the apply step's "10 when the foot touches" x 0.75). That gives joint
stiffness 0.36 on hips and knees and 0.92 on the ankles at 60 Hz. A limp body: strength 0.09,
damping 5. A second, already fallen body: every joint 19.95 and 4.467 with muscle stiffness 1.
Joint order in the character's own list: 0 hip L, 1 knee L, 2 ankle L, 3 hip R, 4 knee R, 5 ankle R,
6-9 spine, then neck and arms (the knee and elbow are the 1-axis ones).

Open, and the reason our ragdoll does not stand yet: how the physics joint turns strength x angle
error into torque (what inertia it multiplies by) and how its stiffness s enters the solve. A
rough estimate from the standing pedestrian's ankle (error 0.17 rad, strength about 85 to 100,
torque needed about 25 N m) puts the multiplying inertia at a few kg m2: far more than the foot's
own, far less than the whole body's.

## How the physics joint uses those numbers (read 2 Oct, evening; structure and numbers)
- **Muscle torque** (the joint's own routine, once per update, dt in):
  1. Current lean / twist angles and the relative spin of the two links about the joint's axes.
  2. Wanted acceleration per axis = strength x (target angle - angle) when the angle drive is on,
     plus damping x (target speed - speed) when the speed drive is on (a mode number on the joint:
     1 speed, 2 angle, 3 both). The twist error is wrapped to +-pi unless a flag says not to.
  3. Overshoot guard, per axis: if `((a x dt + speed) x dt + angle - target) x (angle - target) < 0`
     (it would pass the target within this update), a = 0.
  4. Torque = (the joint's response matrix) inverse x a: the response is how much relative
     angular acceleration a unit torque gives, built from the two links' own response matrices.
     So the torque is whatever makes the joint accelerate by `a` if nothing else acted.
  5. Each axis is clamped to the joint's torque limits, then the torque goes on the child link
     and its opposite on the parent link.
- **Joint stiffness s** scales the links' responses: a real planted pedestrian's foot link had a
  rotational response of about 1.9 to 2.5 per kg m2 where a free 0.8 kg foot would have about 55,
  i.e. (1 - s) of it with s = 0.9655 (the value on its ankle joint: strength 144, damping 24,
  muscle stiffness 10, 60 Hz; the formula gives exactly that, and the torque limit 300 / (1 - s) =
  8700 was there too). So a stiff joint answers every push (1 - s) times as much, and its muscle,
  sized by the same response, still gets its wanted acceleration.
- **Gravity opposition** (a pass over the whole character before the muscles): each part's mass
  and mass x position are summed along the tree, away from the root for ordinary joints and
  TOWARD the foot for the joints of the supporting leg(s) (a joint range the balancer sets), so
  that every joint knows the weight it carries. The torque that holds that weight about the joint
  is applied directly (child +, parent -), limited to `strength x a scale x the joint's
  oppose-gravity factor` (factor 1 by default; the balancer sets 2 x (ground normal . foot up)^2
  on planted ankles). With both legs supporting, the two legs' sums are averaged. The ankles get
  extra trimming by the foot's contact shape. Hinges use only the component about their axis.
