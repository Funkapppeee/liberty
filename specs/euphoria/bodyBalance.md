# bodyBalance (IV's NmRsCBUBodyBalance) — spec draft 1 (7 Oct 2026)

What the arms, spine and head do while the dynamic balancer keeps a character on his feet: arms
swinging with the legs, arms thrown out after a push, the spine following the hips, an optional head
look and body turn. The legs are the balancer's (balancer.md); this behaviour never writes them.

Clean room: written from IV's behaviour (reading the structure, running its routine in the harness
`re/vec_bodybal.py`), not from its code. Numbers still to be confirmed by vectors are marked OPEN.

## Where it lives
Class table 0xfe7de4 (behaviour slot 2). Activate 0x00858480, tick 0x00858a80 (tick(dt)),
deactivate 0x008589f0. Message `bodyBalance` (dispatcher case at 0x0082a94a); the message's
settings are copied into the object at +0x14 .. +0xab.

## Settings (object offsets)
| offset | setting | notes |
|---|---|---|
| +0x14 | bodyStiffness | arms' and spine's muscle strength (stiffness) |
| +0x18 | elbow | how much the elbows follow the hip swing |
| +0x1c | shoulder | how much the shoulders follow the hip swing |
| +0x20 | damping | damping ratio for the muscles it sets |
| +0x24 | maxSteps | handed to the balancer |
| +0x2c | headLookPos | vec3 |
| +0x38 | headLookInstanceIndex | int, -1 = none |
| +0x3c | spineStiffness (hash 0xadea4bb3, name inferred) | 10 in the real game; the spine's strength is its square (confirmed); also feeds the head look's stiffness (+1) |
| +0x40 | headLookAtVelProb | chance of looking along the body's velocity |
| +0x44 / +0x48 | somersaultAngle / Threshold | forward tilt that throws the arms out |
| +0x4c / +0x50 | sideSomersaultAngle / Threshold | sideways tilt |
| +0x54/+0x5c/+0x64 | angVelMultiplier (x, y, z) | stored interleaved with the next |
| +0x58/+0x60/+0x68 | angVelThreshold (x, y, z) | |
| +0x6c | armsOutOnPushMultiplier | |
| +0x70 | armsOutOnPushTimeout | copied to the timer +0x114 at activate |
| +0x74 | returningToBalanceArmsOut | factor used when the body is already turning back upright |
| +0x78 | armsOutStraightenElbows | |
| +0x7c | armsOutMinLean2 | lower clamp for the shoulders' lean2 |
| +0x80 | spineDamping (hash 0x4f4afe22, name inferred) | 1.0 in the real game; the spine's damping ratio (confirmed) |
| +0x84..+0x98 | turnOffProb, turn2TargetProb, turn2VelProb, turnAwayProb, turnLeftProb, turnRightProb | body turn choice |
| +0x9c | elbowAngleOnContact | elbows bend at least this while a hand touches |
| +0xa0 | bendElbowsTime | length of the bend after a contact |
| +0xa4 | bendElbowsGait | lowest elbow bend during the swing |
| +0xa8 / +0xa9 / +0xaa | useHeadLook / armsOutOnPush / useBodyTurn | flag bytes |

## What the real game sends (1 Oct session, `re/bodybal_params.py`: 13 runs, 14 distinct blocks)
bodyStiffness 9, elbow 1.75, shoulder 1.0, damping 0.7, +0x3c 10, +0x80 1.0, maxSteps 100,
somersault 1.0 / 0.25 both ways, angVelMultiplier (4, 1, 4), angVelThreshold (1.2, 3, 1.2),
armsOutOnPush on in 10 of 14 (multiplier 1.0, else 0.0; timeout 1.1, or 1.0), returningToBalanceArmsOut 0,
armsOutStraightenElbows 0, armsOutMinLean2 −9.9, turn probabilities 0.1 / 0.2 / 0.3 / 0.15 / 0.125 / 0.125,
elbowAngleOnContact 1.9, bendElbowsTime 0.3 (0.5 twice), bendElbowsGait 0.7, useHeadLook and useBodyTurn
on in 11 of 14, headLookAtVelProb drawn per message (0.08–0.98, or −1 = never), headLookPos set in 11.
One block differs throughout (bodyStiffness 8.7, somersault 0.5, angVelMultiplier (3.3, 0.6, 3.3),
armsOutMinLean2 −2.8, +0x3c 9.2, +0x80 0.9): probably another ped type.

## State it keeps
+0x114 arms-out timer (starts at armsOutOnPushTimeout, counts down), +0x118 a fixed lower bound for
the arms-out angle terms, +0x11c/+0x120/+0x124 the body's three tilt angles from last tick (for their
rates), +0x128 elbow-bend timer, +0x12c a random elbow value, +0x130 last seen balancer step count,
+0x134 head-look timer, +0x138 / +0x13c / +0x140 random draws for head look and body turn,
+0xd0..+0xfc the body frame, +0x144 flags (1 running, 2 a frame was supplied, 4 balancer gone,
8 looking along the velocity).

## Each tick (confirmed against IV's routine: 500 random cases, 27,500 values, `crates/euphoria::bodybalance`)
Left / right are his own. "Planted" = a foot's contact bit 0x04 set and bit 0x10 clear.
1. **New step:** when the balancer's step count (balancer +0x1d4) is non-zero and differs from the
   last seen, remember it and draw the elbow value e = 0.8·u (one draw).
2. **Gains** (strength = s², damping = 2·s·ratio): shoulders and wrists s = bodyStiffness, ratio =
   damping; elbows s = 0.9·bodyStiffness, ratio = 0.8·damping; spine0..3 s = spineStiffness, ratio =
   spineDamping; neck and head s = bodyStiffness, ratio 1.0. Muscle stiffness 1.5 on spine0..3, neck
   and head every tick. (Real values: arms 81/12.6, elbows 65.6/9.07, spine 100/20, neck 81/18 —
   the same as faerost's panel.)
3. **Speed gauge** X = clamp(√(2·KE), 0.001, 1), KE = the body's kinetic energy per kg (the same
   routine catchFall uses).
4. **Body angles** from spine2's part rows (IV keeps them as a = up, b = left, c = back on an upright
   character): pitch = asin(clamp(c.z)), then with k = 1 / cos(pitch): roll = asin(clamp(b.z·k)),
   heading = asin(clamp(−c.y·k)). Rates = (angle − last angle) / dt; the angles are kept.
5. **Arms-out amount** A = −mx·S(pitch rate, tx)·r1 − my·S(heading rate, ty) − mz·S(roll rate, tz)·r2,
   S(r, t) = clamp(|r|, t, 7) − t, (mx, my, mz) = angVelMultiplier, (tx, ty, tz) = angVelThreshold.
   The somersault terms (somersaultAngle × a forward tilt, sideSomersaultAngle × a side tilt) and the
   tests that pick r1 / r2 use tilts measured from the **character's** rows, which IV sets once per
   character (right, up, back): with those the tilts are exactly 0, so the somersault terms vanish,
   r1 = 1 and r2 = returningToBalanceArmsOut. (Dead in shipped IV, like the brace's F.) A ≤ 0.
6. **Timers:** if the arms-out timer > 0: timer −= dt and the push multiplier m =
   armsOutOnPushMultiplier, else m = 0. If the bend timer > 0: bend −= dt. If either hand touches
   anything: bend = bendElbowsTime.
7. **Spine:** lean1 of spine0..3 = −0.07, 0.25, −0.07, 0.07; lean2 = 0 (the side tilt × 0.25, ×2 on
   spine0/1, which is 0 as above); twist = (0.15·(hipL wanted lean1 − hipR wanted lean1) + (hipL
   lean1 − hipR lean1)) × 0.35, 0.3, 0.2, 0.15. Neck and head wanted angles 0.
8. **Arms**, left then right, each only if the game doesn't hold that arm and **either** foot is
   planted (o = the other side's hip, m = this side's hip):
   - shoulder lean1 = shoulder·((o.want1 − 1.2·o.now1) − 0.6·(1.1·o.now1 − m.now1)) + shoulder's
     current lean1; shoulder twist −0.35;
   - elbow = elbow·(o.want1 − o.now1) + elbow's current angle; with armsOutOnPush add
     armsOutStraightenElbows × A (left) or × (A + the left arm's push P, below) (right — a quirk);
   - while the bend timer > 0, with n = the COM velocity's direction: if (n·back > 0 and n·right > 0
     for the left arm, < 0 for the right) or n·back > 0.5: elbow ≥ elbowAngleOnContact; otherwise
     the shoulder's lean2 gets the cap e (the elbow value);
   - elbow clamped to bendElbowsGait .. 6;
   - shoulder lean2: base = 0.3·m.now2 + 0.5. With armsOutOnPush: P = −m·(0.5·F(o.want2) +
     0.25·F(m.want2) + 0.5·F(o.now2) + 0.25·F(m.now2)), F(x) = clamp(|x|, f, 10) − f with f the
     floor at +0x118 (0 seen so far — OPEN in the live game); v = base + A + P; lean2 =
     armsOutMinLean2 if v < it, else min(v, base − cap). Without armsOutOnPush lean2 = base.
9. Wrists 0.
10. **Ease to neutral:** w = clamp((1 − X)·0.8 + 0.2, 0, 1); every upper-body joint not held by
   another behaviour (clavicles, shoulders, elbows, wrists, spine, neck, head; joints the step
   didn't set keep what they wanted) becomes (1 − w)·value + w·neutral. Neutral is 0, except the
   shoulders' lean2, 0.5. A still body (X → 0) goes straight to neutral; at full speed it keeps 80%.

## At activate
The arms-out timer = armsOutOnPushTimeout; the body angles are taken (as in step 4); three draws:
e = 0.8·u, then the head-look and body-turn draws (u each); the bend and head-look timers 0.
Gravity opposition: spine0..2 and both clavicles 2, neck and head 1. The clavicles' lean2 is set to
−0.4 unless another behaviour holds them. maxSteps is handed to the balancer.

## Not ported yet (OPEN)
- Head look (useHeadLook, on in 11 of 14 real messages) and body turn (useBodyTurn, 11 of 14): both
  take draws, so with them on our draw sequence differs from IV's after the first decision.
- Balancer gives up: stops the head look, and starts catchFall with arms bodyStiffness × 5.5/9, legs
  × 10/9, back × 15/9 (5.5 / 10 / 15 at the real 9).
- The step draw when armsOutOnPushTimeout is the 999 marker (balancer +0x9f0 = 0.3..1.5).
- The +0x118 floor's live value.

## Harness notes
- 0x004e85b0 is encrypted on disk: a cdecl float(float) with its result in st0. Its call sites match
  activate's clamp(−1, 1) + asin, so the harness stands it in as clamped asin.
- The decompiler loses several stack temporaries (the speed gauge X, the elbow-on-contact floor):
  read them in the emulator at the instruction that uses them, not from the C.
- out/ghidra/callpops.json had 8 for the argument-free is-running call at 0x858ae9, which shifted the
  stack by 8 under the stub; the harness forces 0 for the sub-behaviours' +0x8 / +0xc / +0x18 slots.
- The sweep runs on the live game's frame layout (spine2 rows up/left/back, character rows
  right/up/back), not the rig's rest-pose rows. Break checks (each must fail): spine twist share,
  the 1.2 hip factor, elbow gain 0.9, bend direction, blend 0.8, push weight 0.25, the lean2 cap,
  the spin cap 7, the returning factors swapped, own-foot-only gating — all caught.
