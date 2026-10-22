# braceForImpact: behaviour spec (draft 1.7)
Reader: the Reader, 27 Sep 2026. Source: GTA IV 1.2.0.59 as observed. Reviewed by a reviewer
(drafts 0 to 1.3). Behaviour and numbers only; the Writer chooses its own structure. **Required order**
marks the places where order is a fact that affects results. Items marked **OPEN** are settled by
test vectors (`out\vectors\brace_*.json`), not by more prose.

## Conventions
- **Axes:** Euphoria runs in GTA's own Z-up world frame. IV sets its gravity to
  (0, 0, −9.8), so there is no conversion inside the behaviour. U = the character's up;
  **F = the character's orientation row at +0x30**. Every place this spec says "facing" means this
  same F. For Bevy, convert once at the edge with a proper rotation, never a mirror.
- **CORRECTION (3 Oct 2026, real memory):** in the 1 Oct session's live Euphoria characters the rows are
  NOT the identity. Each character's rows are set once when it starts and stay constant: +0x10 = his
  right, +0x20 = up, +0x30 = **backward** (horizontal), checked against the thighs' left/right order
  over 4,000 samples (re/mg2 capture, `nmchar` records). So F below is his back direction, not U. The
  vectors (made with F = forward, −forward and U) still pin the routines; what changes is what Liberty
  must feed them (brace, catchFall's fwd/face, headLook). Re-wired 7 Oct 2026 (`RagdollSim::nm_rows`, taken from the
  shoulders when the ragdoll starts): brace F = backward; catchFall fwd = up, face = backward; headLook only reads
  up, which was already right.
- **F in the shipped game (old reading, draft 1.4, superseded above):** F starts as identity row 3, (0, 0, 1),
  which is **U itself**. No code in NM or in GTA's side has been found writing it, including through
  the encrypted matrix helpers. If it stays U, then (d with U removed)·F = 0 and F·ĥ = 0 always, so
  the front hold-off never fires, the torso bend is b = 0.2 flat, and the head-look test's dot is
  about 0. The vectors with F = U confirm exactly that: the hold-off never fires, identical to
  timeToBackwardsBrace = 0. These are probably dead paths left over from the library's Y-up default.
  Consequences with F = U, spelled out:
  - The torso term facing·ĥ is **exactly 0**, because ĥ is horizontal. So while bracing,
    **b = 0.2**, constant.
  - The head-look test uses the full 3D direction, so dot(dir, U) ≤ 0.7 holds unless the target is
    more than about 45° above his chest. The look-ahead branch therefore passes that test except
    for targets well overhead.
  - The front hold-off never fires.
  **Writer:** implement F as a character input that defaults to U (faithful to IV), so both
  readings stay testable. F = the ped's real forward is the designers' apparent intent, and a
  one-line switch.
  (a reviewer's hypothesis b; still OPEN until a writer is ruled out.)
- **The character provides:** centre of mass position and velocity; an **up** unit vector (against
  gravity); F; per-part world position, orientation and velocity; and a per-part "in contact"
  flag. Contact counts only when it is not contact with the
  character himself.
- **dt** is the Euphoria step the engine passes in (not the render frame). **OPEN**: whether it is
  fixed, and the behaviour when 2·dt > 1.
- **clamp(x, lo, hi):** in the places noted, the game takes the lower bound first and the upper
  bound second; if lo > hi, the upper bound wins. **OPEN** per site, pinned by the reversed-bounds
  vector.

## Parameters
| name | type | RDR default | meaning |
|---|---|---|---|
| bodyStiffness | float | 12.0 | overall stiffness; many settings are derived from it |
| pos | vec3 | – | brace target point, in the **local frame of object `instanceIndex`** |
| instanceIndex | int | -1 | object `pos` and `look` belong to (-1 or invalid: world frame) |
| braceDistance | float | 0.65 | distance at which to brace |
| targetPredictionTime | float | 0.45 | look-ahead on the object's approach, in seconds |
| reachAbsorbtionTime | float | 0.15 | how much of the approach the body gives way to, in seconds |
| look | vec3 | – | head-look target (same frame as pos) |
| grabStrength, grabDistance, grabReachAngle, grabHoldTimer, grabDontLetGo | – | – | passed through to grab |
| timeToBackwardsBrace | float | – | length of the early hold-off window, from Start (see Brace decision) |
| legStiffness | float | – | **OPEN** |

A message whose `start` is false only updates parameters; it does not restart the behaviour.

## Random generator (required order; verified by vector)
- There is one generator per character, and other behaviours use it too.
- Each draw steps a 32-bit multiply-with-carry first: `t = x·0x5CDCFAA7 + c` (64-bit), then
  `x = low32(t)` and `c = high32(t)`.
- The draw is then taken from the **new** x: `u = (x & 0x7FFFFF)·2⁻²³`, which lies in [0, 1).
  A "coin" is bit 0 of the new x, and it uses up the draw.
- Every draw listed below happens in exactly the order given. One extra or missing draw desyncs
  everything after it.
- **Verified:** 10 of 10 activation vectors are bit-exact (5 seeds × 2 character kinds), including
  the case with 6 draws.

## Verified by multi-frame vectors (`out\vectors\brace_frames_*.json`)
- Idle (target 10 m away, still): frame 1 uses 4 draws (A, B, look timer, coin), and every frame
  after that uses exactly 2 (A, B). The look timer drops by exactly dt per frame.
- S goes from 100 straight to P on the first frame (the min rule).
- An 11 m/s car is tracked as 10 m/s (the clamp). P = L − 10·0.45 on frame 1 exactly
  (9.817 → 5.317).
- **Arm choice (vectors with Niko's rest pose from player.wft):** a target approaching from his
  left raises only the left arm, one from his right raises only the right arm, and one from straight
  ahead or behind raises both. This holds with F set either way. At 8 m/s from 8 m with
  timeToBackwardsBrace = 0, the brace starts on frame 27–28, decided by S alone (P < 0.65 once
  L < 4.25 m). With 0.5, a +F-side target is held until frame 29–30.
- **Frames (corrected):** IV sets Euphoria's gravity to (0, 0, −9.8) at startup, so NM runs
  **Z-up in the same world frame as GTA**. The Y-up value in draft 1.2 was only the library default.
  Inputs therefore need no axis conversion inside the behaviour. U is the character's up (0, 0, 1)
  when standing.
- **F:** vectors exist for F = +forward, −forward and U. The arm choice is identical in all three.
  With F = U the hold-off never fires. See Conventions for why U is the best reading.

## Draws per frame, in order (required)
On-feet frame:
1. D = 0.1·u, only on a frame where both arms are on **and** the "fresh" flag is set. The fresh
   flag is set at Start and on every not-bracing frame. It is cleared on any bracing frame, after
   this check, and on any frame with exactly one arm on. So D is drawn at most once per brace, and
   only if both arms come on together on the first bracing frame; an arm that drops out and comes
   back does not redraw it.
2. A, B again (2 draws): on **every** frame that is not bracing, not only on the transition.
3. Look timer, then coin (2 draws): whenever the timer is ≤ 0. The timer is 0 after Start, so the
   **first on-feet frame always draws them**.
The falling branch runs instead of the on-feet frame, never in the same frame, so its draws do
not mix with these. **On the failure frame:** 1 draw if it hands off to catchFall, 2 if it hands off
to rollDownStairs (coin, then random factor). The grab check (see Falling) runs on that same frame,
after the hand-off.

## Character kind
The character kind comes from the rig asset's name: 1 = Fred (male peds, `Fred.art`),
2 = Wilma (female, `Wilma.art`), 3 = Cowboy, 0 = other (confirmed). Wilma uses the smaller
hand-spread range:

| | kind 2 | other |
|---|---|---|
| hand spread A (left), B (right) | 0.075 + 0.155·u | 0.17 + 0.23·u |

## Start
1. Store the parameters and enable every limb at full blend weight.
2. **Target tracking:** convert `pos` to world and store it as the previous target. Zero the target
   velocity, lean, arm aim and timer state. The smoothed distance S starts at **100**.
3. **Balancer:** counter 100, lean −0.22, and the current `bodyStiffness`; start it if it is not
   running. Head-look is started with its "keep" flag.
4. **Random draws, in this order:** A, B, then the hip bend h = 0.02 + 0.08·u, then the side bias
   s = u − 0.5, then the torso factor w = 0.2 + 0.4·u. If h < 0.05, draw w again as 0.2 + 0.15·u.
   That is **5 draws, or 6 when h < 0.05**.
5. Remember `bodyStiffness` so later changes can be detected.

## While on his feet (the balancer has not failed)
### Sensing (required order: the velocity is computed before the previous target is replaced)
- The target is `pos` converted to world. Its velocity is (target − previous)/dt, **clamped to a
  length of 10 m/s**. Then previous ← target.
- Relative to the **spine2 part's position** (the chest; confirmed), the target lies at distance L
  in direction n (unit, pointing **from him to the target**; corrected in 1.5, it was backwards
  before, as a reviewer caught from the vectors). With that n, v_c below is positive when the target
  is closing in.
- The closing speed is v_c = (his velocity − target velocity)·n.
- The predicted distance is P = max(L − v_c·targetPredictionTime, 0.1).
- The give distance g = clamp(v_c·reachAbsorbtionTime, lo = L − braceDistance, hi = L − 0.3).
  The body's lean-away vector is −g·n.

### Brace decision
- Smoothing, with a = 2·dt: S ← min((1 − a)·S + a·P, P). S jumps down immediately and rises
  slowly.
- He is bracing when S < braceDistance + (0.1 if he was already bracing, else 0).
- **Early front hold-off (confirmed by vectors):** while the time since Start is less than
  `timeToBackwardsBrace`, a brace is cancelled whenever the target is on the **+F side**. The test
  is (target − centre of mass, with its U component removed)·F > 0. Both arms go off on those
  frames. With F = U (see Conventions) this never fires. The time since Start accumulates dt on
  **every** frame the behaviour is active, on feet or falling. A `start = false` parameter update
  does not reset it; only Start does.
- **Measuring point (confirmed):** L and n are measured from the spine2 part's position. The
  closing speed uses the centre-of-mass velocity. g is always the full clamp. In the test cases so
  far v_c·reachAbsorbtionTime happened to be below L − braceDistance, so g came out as
  L − braceDistance, but that's a property of those cases, not a rule.
- When he is not bracing, both arms are off.

### Which arms (only while bracing)
- For each arm, take the direction from that arm's shoulder to the target, remove its component
  along up, and normalise it. Measure angle = acos(clamp(dot with the **side axis**, −1, 1)).
- **Side axis (confirmed):** the spine2 part's **second orientation row**, with its U component
  removed. In Niko's rest pose it points to **his left**, −X in model space. It doesn't depend on F,
  which is why flipping F in the vectors never swapped the arms.
- Left arm margin m = 2.8916 − angle (that is, π − 0.25). Right arm margin m = angle − 0.25.
- An arm switches on when m > 0.2 and switches off when m < −0.2, so each arm has its own
  hysteresis.
- An arm flagged unavailable by the game (it's holding something) never switches on. Instead that
  hand gets an IK hold with strength `bodyStiffness` and 1.5.
- **Leading hand:** at the moment both arms become active, draw a delay D = 0.1·u, and mark whether
  the left hand is the one nearer the target. The nearer hand reaches at once; the other waits
  until D has run down by dt.
  **Timing (required order, 1.5):** in each on-feet frame, the waiting hand checks first. It
  reaches this frame only if D < 0 (strictly). Then, after the hands have been aimed, D decreases by
  dt, and only while D ≥ 0. So a delay of D takes ⌈D/dt⌉ + 1 frames before the second hand moves.

### Arm reach (the arms aim with IK, not fixed angles)
For each arm that is on and not waiting:
- Its elbow stiffness is (`bodyStiffness`, 0.75), and its wrist stiffness is
  (`bodyStiffness` − 1, 1.75).
- **Hand target:** start from the target point pushed sideways, across the line from him to the
  target, by ±0.5·spread (the left arm uses A, the right arm uses B, on opposite sides; the push
  applies only while both arms are on). Then shorten it along the shoulder-to-point line to a
  reach of min(distance + 0.05, R_max). The 0.05 is dropped when that hand is already in contact.
  R_max is 0.55, or **0.65 while both arms are on and spine twist is not being held back**.
- Solve two-bone IK for the arm toward that hand target, blending with the hand's velocity.
- The wrist is set to lean1 0.4, lean2 −0.4 and twist 1.0.
- **Wrist IK:** if the hand is in contact with something (the forearm or hand part), or it is
  within **0.5 m** of the target, the wrist is also aimed at that contact point or target. If the
  contact is on the brace object itself, mark "touching" for that hand.
- The exact IK maths is **OPEN**; it will be written as its own spec (it is shared with other
  behaviours). IK inputs so far:
  - Joint data comes from IV's own rig file `pc/data/naturalmotion/Fred.art` (Wilma.art for female
    peds): per joint, the limits, muscle block, position and twist and lean axes.
  - The IK needs the arm's **base frame (settled, 28 Sep, draft 1.7)**: a rig constant per limb, not
    a world frame. It is the elbow joint's frame expressed in the shoulder joint's frame, both taken
    from the rig: rotation = the elbow joint's axes in shoulder-joint coordinates; translation = the
    elbow's position in shoulder-joint coordinates (its length is the upper-arm length, 0.288 m for
    Fred). The legs use the same construction (knee in hip, 0.416 m). Observed in the running game
    on active characters and matched to < 1e-4. The earlier clavicle-frame and identity readings
    are **withdrawn**. With the right base, the brace's elbow targets vary with the situation
    (about −0.11 to +0.80 rad in the reference cases) instead of a constant; hand targets sit just
    past full reach (0.58–0.65 m), so the arms end up nearly straight but still aimed.
- **Placeholder, OPEN:** an arm that is off holds its current pose until the vectors settle the
  relaxed pose.
- The ±0.5·spread push depends only on both arms being **on**, including while the second hand is
  still waiting out D.
- "Spine twist is not being held back" means that spine twist's own hold flag is clear.
  **OPEN**: what sets that flag.

### Balance and torso
**Required order:** the lean step reads the spine bend b left over from the previous frame and
decays it; the torso step (only while bracing) then sets a new b for the next frame. On frames
without bracing, b only decays. Keep `b·(−4)·(1/6)` and catchFall's `bodyStiffness·6·(1/12)`,
`·9·(1/12)`, `·15·(1/12)` in exactly
these forms, since rewriting them changes the last bits.
- The balancer gets the unit direction from the centre of mass to the target and runs its **lean**
  step:
  - The spine bend amount b is spread over the four spine joints: two of them get
    (1.5 + s)·0.25·b and the other two get (1.5 − s)·0.25·b: **spine_0 and spine_1 get (1.5 + s), spine_2 and spine_3 get (1.5 − s)**, as lean1 (settled 28 Sep from the torso vectors).
  - The balancer's hip pitch is b·(−4)·(1/6).
  - Then b decays: b ← b/(1 + 3.18·dt).
- **While bracing:** hip bend = −h. If `bodyStiffness` changed since it was last applied, re-apply it
  to the balancer. Then run the torso step:
  - Spine twist aims at the target and follows the target's velocity.
  - The spine bend is set: b = (facing·ĥ)·w·Δz + 0.2, where ĥ is the horizontal unit direction from
    the target to his centre of mass, and Δz is the up-axis height of the pelvis part above the
    target.
  - When both arms are on, spine twist starts with stiffness `bodyStiffness`; otherwise it stops.
- **While not bracing:** draw A and B again (same table, 2 draws), set hip bend to 0, and stop spine
  twist.

### Head
- A look timer counts down by dt. When it reaches ≤ 0, reset it to 0.25 + 1.25·u (1 draw), then
  set "look ahead" from a coin (1 draw). **Required order:** timer draw, then coin draw.
  **Timing (1.5):** subtract dt first, then check: timer ← timer − dt; if timer ≤ 0, redraw. The
  timer is 0 after Start, so the first frame gives −dt ≤ 0 and redraws.
- If "look ahead" is on, he is bracing, at least one arm is on, and dot(direction to target,
  facing) ≤ 0.7, using the **full 3D** unit direction from the centre of mass to the target: look at 0.83·pos + 0.17·look (object-local, converted to world), with no object
  attached. Otherwise look at `look` on object `instanceIndex`.
- Head-look stiffness is `bodyStiffness` − 1.

## Matching tolerance for vectors
- **Bit-exact:** random draws and generator state, draw counts, arm on/off, bracing on/off, the
  feedback bits, S, P, g, L, and the target velocity. **sqrt is reproducible:** the game runs x87
  `fsqrt` on a 32-bit float and stores the result back as a 32-bit float, which gives the correctly
  rounded f32 square root, the same as IEEE `sqrtf`. Use an f32 sqrt and the same operation order,
  and L (and so S, P and g) can match bit for bit. The sums and dot products feeding it
  (dx² + dy² + dz², v_c, (1 − a)·S + a·P) are built with **SSE single-precision** instructions
  (`mulss` and `addss`), so each step rounds to f32. Plain f32 maths in the same order is right; don't
  use f64.
- **Within 1e-5 relative:** anything that goes through acos, atan or the vector normalise (angles,
  unit vectors). Those come from the C runtime or from the encrypted normalise, which the harness
  only stands in for.
- **Within 1e-3:** IK joint targets, hand targets, and anything downstream of the solver.

## Once the balancer has failed (falling)
"Failed" is the dynamic balancer's own report, not a condition this behaviour evaluates. The
balancer says it is no longer running **and** it has flagged a fall. The balancer spec will define
it.
Runs once, at the moment of failure (verified by vectors, `out\vectors\brace_fall.json`: 6 seeds ×
5 contact cases, all consistent):
1. Stop the balancer, spine twist and head-look. Pedal also stops if grab isn't holding.
2. Draw a coin: **1 draw, always**, taken before any contact test.
3. **Choose (corrected in 1.6):**
   - **coin = 0** → catchFall, whatever the contacts.
   - **coin = 1** → rollDownStairs, **unless** at least one foot is in contact **and** nothing else
     counts (no upper-body part, no pelvis, thigh or shin in contact). In that "only feet" case it's
     catchFall. So one or both feet planted with nothing else touching gives catchFall; no feet
     touching, or anything above the feet touching, gives rollDownStairs. "No feet touching"
     includes being **fully airborne** (thrown by a car, no contacts at all): coin 1 gives a roll.
   - "Upper body" means any part in the character's "ub" body mask.
4. **rollDownStairs** settings (confirmed): stiffness clamp(bodyStiffness − 3, 1, 15), then 0.4, 0,
   random factor 0.2 + 0.6·u (**1 more draw**, so 2 in total), **−0.9** (draft 1.5 wrongly said
   −1.2), 1.4, 0.2 and 0.6, with two flags set. **OPEN**: map these to rollDownStairs parameter names
   (for its own spec).
5. **catchFall** settings block (confirmed at bodyStiffness 6, 12 and 18): legsStiffness
   bodyStiffness·6·(1/12), torsoStiffness bodyStiffness·9·(1/12), armsStiffness bodyStiffness·15·(1/12) (keep
   this multiply order), then forwardMaxArmOffset 0.35, backwardsMinArmOffset −0.25, 0 and a flag byte
   (names from NaturalMotion's parameter list via RDR's behaviours.xml, matched by unique defaults). No extra draw.

Every frame while falling:
- If a hand is touching the brace object and grab is not holding, start grab on that hand, or both,
  with the grab parameters.
- While grab holds, run pedal with stiffness clamp(bodyStiffness − 1, 1, 20), counter 100, and
  0.02, 4.0, 0.3 and 0.3.

## Stop
Stop the balancer and head-look. If the fall branch ran, also stop spine twist, grab, catchFall and
pedal. Report "brace".

## Feedback
Each frame, "brace" is reported with bits for left arm on, right arm on, left changed and right
changed. "Changed" compares against the value saved **before** this frame's decision.

## Writer questions (part 1, 27 Sep 2026)
Part 1 (`crates/euphoria`, module `brace`) passes all 10 cases of `writer_brace_part1.json`
bit-exactly. These choices were not pinned by the spec or the vectors (F = U, no hold-off, no
drop-outs in them); each is what the Writer picked, to confirm or correct:
Q: Hold-off timing: the Writer adds this frame's dt to the time since Start **before** testing
   it against timeToBackwardsBrace. Is the test made with the value before or after the add?
Q: When the hold-off cancels a brace, the Writer treats the frame as fully not-bracing: A and B
   are redrawn, the fresh flag is set, and next frame's hysteresis sees "was not bracing" (no +0.1).
   Is that right, or does the cancel only switch the arms off?
Q: D decreases by dt on every bracing frame while D ≥ 0, even when no arm or one arm is on
   (the one-arm vectors show 0 → −dt, so at least that case is right). It is never reset when a
   brace ends; it is only redrawn. Does anything reset D, and does it tick on bracing frames with
   no arm on?
Q: Side axis: the Writer normalises spine2's second row after removing its U component, before the
   acos. Is it normalised, or used at its reduced length?
Q: The left-arm constant: stored as exactly 2.8916 (f32), or computed as π − 0.25?

## Writer questions (part 1, 28 Sep 2026, after the regenerated pack)
All 10 cases of the regenerated `writer_brace_part1.json` now pass bit-exactly. The fix was **not**
in the D path, which already drew D = 0.1·u when both arms came on with the fresh flag set. The
+Y/−Y cases in the same pack exercise that path (frame 28: 1 draw, both arms, D → −0.0031) and
passed before the change. In the four side cases the Writer's code turned on **only one arm**, so
D was never due. The change was to stop normalising the side axis: spine2's second row is now used
at its reduced length after its U component is removed (this answers Q4 above the way the vectors
need, pending confirmation).
Q: The pack's `spine2_row2` is (−0.2214, 0, −0.9752), which still points **mostly down**, so the
   rig fix in the Reader note does not seem to have reached this file. With that row, a target
   straight off his left gives |dot| ≈ 0.22, an angle of about 1.35 rad, and both margins above
   0.2, so both arms come on. That contradicts "a target approaching from his left raises only the
   left arm" (Verified by multi-frame vectors). Is this pack still from the old rig, or is the arm
   choice claim wrong?
Q: Does the game really use the side axis unnormalised, or is the normalise one of the encrypted
   helpers the harness stands in for, with the stand-in not being applied here? With a correct
   (horizontal, unit) spine2 row the two readings agree, so a regenerated pack that has a
   horizontal side axis **plus** one tilted side-axis case would settle it.

## Reader note (28 Sep, night): vector pack part 1 regenerated
The Reader's test rig had a wrong spine2 frame (its side axis pointed mostly down). With that fixed,
`writer_brace_part1.json` was regenerated. The Writer's part 1 now passes 6 of 10 cases; the four
side-approach cases (from −X and +X, both hold-off values) fail at frame 28. No spec change: in the
new cases **both arms turn on in the first bracing frame**, which the old pack never exercised, and
the existing rule applies: D = 0.1·u is drawn on that frame (one draw; the vector shows D = 0.01353,
then −dt at the end of the frame → −0.0031), and both arm flags read on, the second hand waiting out
D. The Writer's code makes no draw there and leaves D at −dt. Please re-run against the new pack and
fix that path. The five Writer questions above are still open; answers to follow.
