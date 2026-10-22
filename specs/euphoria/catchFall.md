# catchFall: behaviour spec (draft 2)
Reader: the Reader, 28 Sep 2026. Source: GTA IV 1.2.0.59 as observed (NmRsCBUCatchFall), checked
against vectors from IV's own code (`out\vectors\catchfall_*.json`: 16 cases × 20 frames with free
hands, 12 cases × 40 frames with hands touching; 800 frames). Parameter names from NaturalMotion's
list (RDR `behaviours.xml`, matched by unique defaults; a reviewer). Z-up (GTA world). Behaviour and
numbers only.

Draft 2 (same day): the touching branch, the speed gauge's scaling of the pose (draft 1's OPEN leg
factor), the feedback events; corrected the wrist rule (keyed on the probe hit, not on touching).

## Parameters (IV defaults; the brace hands over the first five)
| name | default | brace hand-off |
|---|---|---|
| legsStiffness | 6 | bodyStiffness·6·(1/12) |
| torsoStiffness | 9 | bodyStiffness·9·(1/12) |
| armsStiffness | 15 | bodyStiffness·15·(1/12) |
| forwardMaxArmOffset | 0.35 | 0.35 |
| backwardsMinArmOffset | −0.25 | −0.25 |
| zAxisSpinReduction | 0 | 0 (the spin-reduction step only runs when > 0; not specced) |
| useHeadLook (flag) | true | from the brace |
Internal constants: maxReach 0.6, minReach 0.4, probeLength 0.7, reachDecay 0.5, speedGain 1.0 (the
"4.9" gravity term's scale).

## Character inputs per frame
- Orientation rows: `fwd` (the character's second row) and `face` (third row), and `up`. In the
  shipped game these rows are the character's +0x20 and +0x30 rows (braceForImpact spec, Conventions,
  correction of 3 Oct): fwd = up, face = his backward. With fwd = up, side = 1 and d = 0, so the arm
  offset direction n is up either way. The formulas below are verified for general rows.
- The chest part's velocity `v_chest`, the COM velocity `v_com`, and the body's kinetic energy per kg
  KE = Σ(½·m·|v|² + ½·(Ixx+Iyy+Izz)·(1/3)·|ω|²) / Σm over all parts.
- Per arm: the shoulder position (upper-arm joint), the hand part's position, whether the arm touches
  something (any part in body mask "ul" / "ur" with its contact bit), and a ground probe
  (segment → hit point, normal).

## Start (once)
- speed gauge g = 1.0.
- Draws, in order (9 in total, verified): mL = (u+1)·0.5; mR = (u+1)·0.5; s = 0.2u − 0.1; then left
  arm, right arm, each: a = 0.4u − 0.4, b = 0.5u, elbowCap = 0.6u + 0.7. Each arm's reach r = 1.0.
- Legs: ankle lean1 −m, hip lean1 m, knee −2m (left mL, right mR).

## Each frame (dt)
1. g ← clamp(g + (2·√(2·KE) − g)·5·dt, 0.2, 1.0).
2. rL' = max(rL, 0.3), rR' = max(rR, 0.3); low = min(rL', rR').
3. Feedback to the game, event name "catchFall" (feedback interface, no arguments):
   - slot 0x10 when low == 0.3 exactly (an arm's reach has fallen to the floor);
   - then slot 0x0C when low < 0.9 and g < 1.0 (sent every such frame).
   **OPEN:** which of success / failure / finish each slot is, and what the game does with them.
4. Stiffness (damping ratio 0.5): upper spine "us" torso·g; neck "un" torso·g (only with useHeadLook);
   left arm "ul" arms·rL'; right arm "ur" arms·rR'; legs "lb" legs·g. Wrists: ((arms + 13)·0.5·r', 1.0).
   Ankles: (10, 1.0).
5. Fall direction: v = v_chest − (average of the two arms' surface velocities; 0 on the ground);
   F = normalize(v − 4.9·speedGain·up).
6. back = −(face·F) (> 0 when falling backward); side = clamp(fwd·up, 0, 1).
7. lean w = clamp(side·clamp(2·(face·(v_com − armAvg)), 0, 1) − 0.5·back, 0, 1).
   Spine joints 0..3: lean1 0.2·w·g, lean2 s. Spine twist *= 1/(1 + 12·dt) unless head-look's hold flag.
8. Legs each frame: hip lean1 (m + w)·g; knee −2m·g; ankle lean1 −m (not scaled).
9. Head look at (a head/COM point + F) when useHeadLook.
10. Arms (left then right):
   - d = normalize(fwd × up) (skip if |d|² ≤ 1e-10). Offset direction n = fwd if back ≤ 0, else
     n = normalize(fwd − d × fwd)·(0.4 + 0.6·side).
   - t = clamp(back, backwardsMinArmOffset, forwardMaxArmOffset) (lower bound first).
   - Probe from P0 = shoulder + t·n to P1 = P0 + probeLength·F.
   - Reach r: not touching → r = min(min(3·g, 1), r + 2·dt); touching → r = max(r − reachDecay·dt, g).
   - Hand target H:
     - not touching: D = (hit point, else P1) − shoulder, limited to |D| ≤ maxReach, and pushed out
       to minReach·D/(|D| + 1e-4) when |D| < minReach; H = shoulder + D.
     - touching: H = (hit + hand)/2 with a hit, else the hand's own position. No reach limits.
   - Arm IK to H (IV's arm IK; see armIK.md). Its arguments also differ when touching (OPEN, with the
     arm IK).
   - If back < −0.3: elbow ≤ elbowCap.
   - Wrist: probe found nothing → lean1 0.3, lean2 −0.5, twist 1.0; probe hit → wrist aimed at the
     surface (IV's wrist IK, 0x008376a0 left / 0x00837830 right; OPEN, with the arm IK).
   - Touching and back > 0: spine joints 2 and 3 lean1 −1.0 (overrides step 7 for those two).
11. If any part in mask "ua" touches: keep pushing the body along −F each frame (**OPEN**: magnitude).

## Vectors
Bit-level: draws and start values. Within 1e-4: g, F, back, side, spine lean1 (all four) and lean2,
hip/knee/ankle, both reaches, probe points, hand targets, the no-hit wrist, and the feedback events.
Harness: `re\vec_catchfall.py` (`touch=` sets the contact bit, part +0x118 bit 0x04, on rig parts).
