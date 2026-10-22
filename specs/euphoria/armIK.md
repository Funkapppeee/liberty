# Two-bone arm IK: behaviour spec (draft 1)
Reader: the Reader, 28–29 Sep 2026. Source: GTA IV 1.2.0.59 as observed (arm IK 0x008371c0, solver
0x0085d170), checked against vectors from IV's own code (`out\vectors\armik_solve.json`: 320 solves,
both arms, every request option; the Rust port matches all within 1.2e-5). Used by braceForImpact,
catchFall, grab, pointArm, shot and others. Behaviour and numbers only. Draft 0's open items are all
settled or withdrawn below.

## Joint angles (every 3-DOF joint)
- Each joint has a parent-side frame P (fixed to the parent part) and a child-side frame C (fixed to
  the child part), both world rows; row 2 is the twist axis. The joint's rotation Q is C expressed in
  P coordinates.
- **Raw angles** of Q: twist about row 2 and a swing split over rows 1 and 0, each "4·atan of the
  quarter-angle tangent": with r = √(w² + z²):
  - twist = 4·atan(z/(w + r)) (w > 0) or 4·atan(z/(w − r)) (w ≤ 0);
  - s1 = (w·y + x·z)/(r(1 + r)), s2 = (y·z − w·x)/(r(1 + r)); swing σ = 4·atan(√(s1² + s2²)), and
    (lean1, lean2) raw = σ·(s1, s2)/|(s1, s2)|;
  - if r² ≤ 1.2e-7: twist 0 and (s1, s2) = (y, −x)/√(1 − r²).
  - Checked against IV's own routine on 200 random rotations (≤ 4.3e-7).
- **Joint angles** (what effectors read and are given): per axis, raw + centre, or −raw − centre on
  an axis with the joint's mirror flag, where centre = (min + max)/2 of that axis's limits. Right
  shoulders and hips mirror lean2 and twist; left ones mirror nothing. Checked against IV's own
  measurement on rotated arms, both sides, to 1e-4.
- **Rest:** the joint angles are 0 in Fred.art's rest pose. IV builds C from Fred.art's joint axes
  (row 2 = the twist vector; row 0 = the lean vector turned about the twist axis by −twistCentre), and
  P so that the rest pose reads 0: rest Q = the rotation whose raw angles are −centre.
- Hinges (elbows, knees): the angle is the twist of Q about the hinge axis (row 2). Bending the
  forearm the natural way reads positive (0 = Fred.art's rest bend, about 0.11 rad).

## Inputs per solve (0x008371c0's arguments)
| arg | meaning | default when null |
|---|---|---|
| 1 | limb (its base matrix; its shoulder, elbow and hand) | — |
| 2 | no-overflow flag (low bit) | — |
| 3 | target (world) | — |
| 4 | twist t | 0 |
| 5 | lead time L | 1 |
| 6 | reference velocity (subtracted in the lead) | none |
| 7 | blend k | 1 |
| 8, 9 | reach limiter: factor f (arg 8), max step m (arg 9); on only with arg 9 | off |
| xmm2 / xmm3 | twist scale τ / elbow scale e | — |
- **Limb base matrix B:** the elbow's P frame in the shoulder's C frame (rows + translation). Its
  translation's length is the upper arm; row 2 is the elbow hinge axis. It's a rig constant (observed
  in the running game; see the brace spec).
- The shoulder's limits give centres and half-ranges; its muscle gives strength s² and damping 2·s·d
  (from setStiffness). The current targets of shoulder and elbow are also read (blend sources).
- Hand = the hand part's position. Base = the part above the shoulder (clavicle): its velocity v_b and
  angular velocity ω_b.

## Solve
1. **Lead** (L > 0): d = target − shoulder; c = damping/(strength + 0.1)·L; spin = (d × ω_b)·c;
   d' = d + spin. target += d'·(|d| − |d'|)/|d'| + spin − (v_b − reference)·c. Then keep the target at
   least 0.1 m out along d̂ (push it out along d̂ if its projection is less).
2. **Reach limiter** (on): δ = target − hand; step = min(damping·m/strength, |δ|) + 1e-4;
   n = max(1 − 0.1/step, 0); g = f, or (f + n)/2 if n < f. target = hand + δ̂·step·(1 − g). The final
   blend weight w = 1/(1.0001 − g).
3. **Forearm:** F = (hand − elbow) in the shoulder's C coordinates (through the elbow's frame and B).
   u = |B.translation| (upper arm), l = |F|. Current bend term ρ = asin(B.row2·(F × B.t)/(l·u)).
4. **Elbow:** D = the target in the shoulder's P coordinates, d = |D|; if d > u + l, scale D to u + l
   (d keeps its value). cos e = (l² + u² − d²)/(2·u·l) → e (π if ≤ −1, 0 if ≥ 1).
   elbow = (π − e)·elbowScale + ρ.
   **Note:** ρ is minus the *current* bend, so the result is the needed bend minus the current one,
   yet the effector takes it as an absolute angle. Aimed at where the hand already is, the solve
   gives 0 whatever the bend. That is IV's behaviour: its elbows settle partway to the solved bend.
5. **Shoulder:** G = B.t + (F rotated by `elbow` about B.row2) (the hand with that bend, in C).
   Qtw = rotation by t·τ about Ĝ. Q = arc(G → D) · Qtw, where arc = normalize(Ĝ × D̂, 1 + Ĝ·D̂).
   (twist, s1, s2) = raw angles of Q.
6. **Twist limit:** if |twist| > the twist half-range: redo step 5 with the twist amount reduced by the
   excess and take s1, s2 from that. The reported twist keeps the first value.
7. **Cone:** c = s1²/h1² + s2²/h2² (lean half-ranges). If c > 1 and no-overflow is off:
   elbow += clamp((1 − 1/√c)·√(s1² + s2²)·elbowScale, −0.5, 0.5).
8. **Output** (blend with the current targets, k):
   lean1 = (centreLean1 + s1)·k + prev·(1 − k); lean2 = (centreLean2 + s2·τ)·k + prev·(1 − k);
   twist = (centreTwist + twist·τ)·k + prev·(1 − k); elbow = elbow·k + prev·(1 − k).
   With the limiter on, each is then mixed as prevL·(1 − w) + value·w with the limiter's own previous
   values. The shoulder and elbow targets are written unless their joint is marked "held".

## The right arm
The solve adds the shoulder effector's stored centres, which for the right arm are the mirrored
(left-side) values. The right shoulder mirrors lean2 and twist. **Callers pass twist scale τ = −1 for
the right arm** (catchFall does), and τ multiplies both lean2 and twist (and the twist amount). That
mirror is exactly the right shoulder's, so both arms reach their targets. A caller passing τ = +1 for
the right arm would drive it to the mirror image in lean2/twist. (Draft 1's first version called this
a bug; that came from a sweep with τ > 0 on both arms.)

## Consumers
- catchFall, per arm: twist t = 1.3987, τ = +1 left / −1 right, elbowScale 1, k 1, limiter (factor
  0.4, max step 200), no-overflow off. Lead L = 0.5 with the body's velocity as reference, except for
  a hand that is touching: L = 0 and no reference.
- braceForImpact: see its spec.
