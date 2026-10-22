# Reach for wound (shot behaviour): spec
Reader: the Reader, 3 Oct 2026. Source: GTA IV 1.2.0.59 as observed: hand choice 0x00862ae0,
hit side 0x008624a0, per-hand start/stop in the shot tick (0x0083e910), reach step 0x00861b00,
hold spring 0x00836d50. Checked against IV's own code in the Unicorn harness:
`re/vec_reach_choice.py` (8,356 hand choices: every part, side offsets either side of both
thresholds, hand states, flags, existing wounds) and `re/vec_reach.py` (480 random reach steps:
wounds on all 21 parts, both hands, hand positions near and far, facings, timers, clavicle flags,
time steps). Port `crates/euphoria/src/reach.rs` matches every case (`tests/reach_vectors.rs`); 11
deliberate breaks of its constants and thresholds each fail the test. Behaviour and numbers only.

## Parts and sides
- The character's rows in the live game (1 Oct session, constant per character from its start):
  +0x10 = his right, +0x20 = up, +0x30 = backward. So "normal − facing" below is the normal plus
  his forward: the aim point is pulled out in front of the wound.
IV's 21-part order (pelvis, left thigh/shin/foot, right thigh/shin/foot, spine0–3, neck, head, left
clavicle/upper arm/forearm/hand, right ...). "Left" is the character's left arm (limb at character
+0x100). The character's rows: +0x10 = right, +0x30 = facing.

## On each hit: where it landed
- **front** = −(facing · bullet direction). Below 0 = shot from behind.
- **lateral** = right · (hit − reference), clamped to ±2. Reference = the hit part's own centre for
  trunk/neck/head hits, the body's centre of mass for limb hits.
- The wound is stored in the hit part's frame (local point and local normal).
- (Shots from the front also run a probe along the bullet through the body that can move the stored
  point to another part — not ported; we keep the entry wound.)

## On each hit: which hand (choose)
1. "From behind" flag = reachForWound on, front < 0, not the neck or head.
2. Leg hits mark that leg (left or right; any of thigh/shin/foot) for the wounded-leg collapse.
3. An arm hit below the clavicle, on an arm with no wound yet, marks that arm hurt: it won't reach
   for anything after that.
4. Default: the **far hand**. The left hand takes hits with lateral > −0.05, the right hand hits
   with lateral ≤ 0.05 (a 10 cm middle band gets both), never a hand on its own wounded arm.
   If one hand is holding something (state 1), the other takes it. "No left / no right" flags veto.
5. A hit on one arm always goes to the other hand. A thigh hit with neither hand busy with a wound
   gets both hands.
6. A hand assigned the wound (unless its arm is hurt) remembers the part and wound and restarts its
   reach timer: left from 0, **right from −0.1 s** (it starts a tenth of a second later).

## Per hand, each tick (timers count up by the time step)
- **Start** when: reachForWound on, not both hands holding something, the time since the hit is past
  timeBeforeReachForWound (0.1–0.2 s in the real game), a wound was newly assigned to this hand,
  its reach timer is under grabHoldTime (3 s in the real game), and the hand is free. Starting
  copies the wound into the hand's record and sets its normal sign to +1.
- **Stop** when a new wound is assigned to it (it restarts on the new one the same tick), its reach
  timer passes grabHoldTime, the hand gets busy, or reachForWound goes off.
- While reaching: the step below.

## The step (plan)
- **Clavicle** (unless locked): wounds on the pelvis, spine0 or spine1 → lean1 = its lean1 maximum,
  lean2 = half its lean2 maximum; anything else → lean1 = half the lean1 maximum, lean2 = 0.6 × its
  lean2 minimum. If the joint has an incoming (animated) pose, it then goes halfway back to it.
- Shoulder, clavicle and elbow get oppose-gravity 1.
- **Wound** in the world = the part's frame applied to the stored point; **normal** = the part's
  rotation applied to the stored normal, times the hand's sign.
- **Aim line** D = normalise(normal − facing): the normal tilted away from where he's facing.
- **away** = the hand's distance from the line through the wound along D (the hand's offset is
  measured along D only when it's in front of the wound, i.e. the along-part is clamped at 0).
- **Arm IK target** = wound + D × min(0.5 × away − 0.05, 0.25). Far off, the hand is aimed up to
  25 cm out from the wound; as it closes in, the aim point slides onto the wound and just under it.
- **Twist**: u = how far up the trunk the wound is (projection on pelvis → spine3, 0 at the pelvis,
  1 at spine3). a = 1 − clamp(u, 0, 0.8) × 2.5 (1 low on the belly, −1 at the chest). If a < 0 it
  is scaled by (1 − min(2.5 × away, 1)), so chest wounds only twist once the hand is close. Thighs
  and shins: a = 1. Left hand twist a + 0.1, right hand a − 0.1.
- Arm IK call: no-overflow off, twist as above, twist scale +1 left / −1 right, elbow scale 1, no
  lead, no reference velocity, blend 1, no reach limiter.
- **Wrist IK**: palm onto the wound, facing the (signed) normal, no forced frame, default weight.
- **Sign flip**: if the wrist IK reports a negative result, the hand's normal sign flips for the next
  step (not for head wounds).
- **Hold spring** (below). The clavicle's halfway blend to its incoming pose is the step's last act.

## Hold spring
- Strength 4, or 0.5 once the shot's loosening timer has run out (its duration is 0 in every path
  we found, so in practice 4; open item).
- Palm = hand centre − sign × 0.02 m × the hand's first row. d = |wound − palm|.
- Nothing at d ≥ 0.3 m. Otherwise w = clamp((0.3 − d)/0.2, 0, 1) × strength;
  F = w² × (wound − palm) + 0.2 × w × (wound part velocity − hand velocity).
  With a time step under 1/60 s, F is scaled by dt × 60.
- F goes on the hand at the wrist joint; −F on the wounded part at the wound.

## In Liberty (ragdoll.rs `shot_reach_hit`, `shot_reach`)
- Rows from the ragdoll at the first hit: right = left→right upper arm, flattened; back = right × up.
- STAND-INS: wound normal outward from the hit part's centre; a bullet from behind moves the wound to
  the point opposite the entry through the part's centre; clavicles are welded (no shrug); the palm's
  2 cm offset is left out; no hand is ever busy; the reach runs while the balancer runs (catchFall
  takes the arms after a fall).
- Arm IK at the shot's shoulder stiffness 12.27, damping ratio 1, lead 0; wrist IK weight 2.4 (IV's
  default). `LIBERTY_NO_REACH` turns it off, `LIBERTY_NO_HOLD` drops the spring,
  `LIBERTY_REACH_DEBUG` logs the hit geometry and each step's hand-to-wound distance.
- First runs (out/runs/shot_1003-152017*): both hands start 0.17 s after a belly hit and come down
  toward the wound to 0.16-0.3 m, elbows only partly bent, then hover near the spring's 0.3 m edge
  until the fall. The arm IK's elbow rule (armIK.md: needed bend minus current bend, so elbows settle
  at about half) keeps them short; real victims' elbows were also only a little bent. Compare with
  real footage before changing anything.

## Open
- The front-shot probe that can move the stored wound.
- What sets the shot's loosening duration (shot +0x194), if anything.
- Which incoming pose the clavicle blend uses in Liberty (we have no NM incoming transforms; the
  port takes it as optional and Liberty passes none).
