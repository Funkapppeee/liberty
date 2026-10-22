# shot (GTA IV's Euphoria "being shot" behaviour): draft 0

Behaviour and numbers only, in our own words. Where things were found (addresses, tools) is in
`_trace.md`. Status: **the message and what IV sends are known; the per-frame behaviour is not
ported yet.**

## What IV has
IV's Euphoria build has two shot messages only: `shot` and `shotRelax`. The later family in RDR
(shotNewBullet, shotShockSpin, shotFallToKnees, shotFromBehind, shotInGuts, shotHeadLook,
shotConfigureArms) is not in IV's message table: in IV those are all settings of the one `shot`
message.

## The `shot` message: settings and where they land
Offsets are bytes from the start of the behaviour's settings block.

| Offset | Setting | Kind | IV's built-in default |
|---|---|---|---|
| 0x00 | normal | 3 floats | 0, 0, -1 |
| 0x10 | headLook (a world position to look at) | 3 floats | 0, 0, 0 |
| 0x20 | hitPoint | 3 floats (+ an int in the 4th) | 0, 0, 0 |
| 0x30 | bulletVel | 3 floats | 0, 0, 20 |
| 0x40 | spinePainTwistMultiplier | float | 0.5 |
| 0x44 | flingWidth | float | 0.5 |
| 0x48 | timeBeforeCollapseWoundLeg | float | 0.3 |
| 0x4C | grabHoldTime | float | 2.0 |
| 0x50 | spinePainTime | float | 1.5 |
| 0x54 | bodyStiffness | float | 11 |
| 0x58 | loosenessAmount | float | 1.0 |
| 0x5C | spinePainMultiplier | float | 1.0 |
| 0x60 | timeBeforeReachForWound | float | 0.2 |
| 0x64 | (not set by any message setting found) | float | 1.0 |
| 0x68 | headLookAtWoundMinTimer | float | 0.25 |
| 0x6C | headLookAtWoundMaxTimer | float | 0.8 |
| 0x70 | headLookAtHeadPosMaxTimer | float | 1.7 |
| 0x74 | headLookAtHeadPosMinTimer | float | 0.6 |
| 0x78 | bodyPart | int | 0 |
| 0x7C | maxSteps (stored + 1) | int | 100 |
| 0x80 | shockSpinMin | float | 50 |
| 0x84 | shockSpinMax | float | 90 |
| 0x88 | shockSpinLiftForceMult | float | 0 |
| 0x8C | shockSpinDecayMult | float | 4 |
| 0x90 | shockSpinScalePerComponent | float | 0.5 |
| 0x94 | flags: 0x01 melee, 0x02 fling, 0x04 crouching, 0x08 useHeadLook, 0x10 addShockSpin, 0x20 randomizeShockSpinDirection, 0x40 alwaysAddShockSpin, 0x80 chickenArms | bits | none |
| 0x95 | flags: 0x01 reachForWound, 0x02 useCatchFallOnFall, 0x04 stableHandsAndNeck; 0x40 = "a new hit arrived" (set when the message carries bodyPart, cleared at the end of the next tick) | bits | 0x01 and 0x80 |

A message with `start` true starts the behaviour; without it only the settings change. A message
that carries `bodyPart` counts as a new bullet.

## What the real game sent (pistol, standing pedestrian; session of 1 Oct 2026)
Read from the game's memory while a pedestrian was being shot:

| Setting | Value sent | From `taskparams.txt` (NMShot_Standing) |
|---|---|---|
| bodyStiffness | 13.5 | shotBodyStiffness 13.5 |
| loosenessAmount | 1.0 | shotLooseness 1.0 |
| maxSteps | 6 | shotMaxSteps 6 |
| grabHoldTime | 3.0 | shotGrabTime 3.0 |
| timeBeforeCollapseWoundLeg | 1.0 | shotTimeBeforeCollapseWoundLeg 1.0 |
| timeBeforeReachForWound | 0.166 | a random value between shotTimeBeforeReachForWoundL 0.1 and H 0.2 |
| headLookAtWoundMinTimer / MaxTimer | 0.1 / 0.75 | shotHeadLookAtWoundMinTimer / MaxTimer |
| headLookAtHeadPosMinTimer / MaxTimer | 0.75 / 1.5 | shotHeadLookAtShooterMinTimer / MaxTimer |
| hash 0a3f8806 | 1.058 | fits a random value between shotSpinePainTimeL 0.5 and H 1.5 |
| hash 2953f83a | 1.535 | fits a random value between shotSpinePainMultiplierL 1.0 and H 2.0 |
| hash 1a9ad24f | 0.5635 | fits a random value between shotSpinePainTwistMultiplierL 0.5 and H 1.5 |
| shockSpinMin / Max | 40 / 80 | not in taskparams (set by the game's task code) |
| shockSpinLiftForceMult / DecayMult / ScalePerComponent | -1 / 4 / 0.5 | not in taskparams |
| flingWidth | 0.5 | default |
| flags | useHeadLook, addShockSpin, reachForWound, useCatchFallOnFall | |
| normal | a unit vector at the wound | |
| hitPoint | the wound, world position | |
| headLook | a world position near the shooter | |
| bulletVel | about 75 m/s along the shot | |

### All 33 real messages of the session (`re/shot_params.py`, `shot_params_mem.json`)
- Always the same: bodyStiffness 13.5, loosenessAmount 1.0, maxSteps 6, grabHoldTime 3.0,
  timeBeforeCollapseWoundLeg 1.0, flingWidth 0.5, offset 0x64 = 1.0, headLookAtWoundMaxTimer 0.75,
  headLookAtHeadPos 0.75 / 1.5. Bullet speed 75 m/s (one at 76.6).
- Random per message: timeBeforeReachForWound 0.102 to 0.2; hash 1a9ad24f 0.5 to 1.89; hash
  0a3f8806 0.51 to 1.89; hash 2953f83a 1.0 to 2.05. The last one's range is the spine pain
  multiplier's (1 to 2, or 1 to 3 for a powerful weapon); the other two are the spine pain time and
  the spine pain twist multiplier, which of the two is which is not settled.
- Two weapon classes seen: 30 messages with shock spin 40 to 80, lift -1, decay 4, per-part scale
  0.5, look-at-wound from 0.1 s; 3 messages with shock spin 100 to 200, lift -2, decay 3, scale
  0.6, look-at-wound from 0.4 s (the POWERFUL_WEAPON set's 0.4).
- Flags: useHeadLook + addShockSpin + reachForWound always (one message without useHeadLook);
  useCatchFallOnFall in half of them (16 of 33).
- bodyPart values seen: 0, 1, 9, 10, 11, 12 (13 times), 14, 15, 16.
- The 3 other blocks seen held the built-in defaults (a behaviour started without a message).

## What starting the behaviour does (structure, from the game's code)
- Resets its timers and per-part state, and sets every joint's "muscle scale" to 1.
- Body stiffness set: upper body = bodyStiffness; two derived stiffnesses = bodyStiffness x 10/11
  and x 20/11 (13.5 gives 12.27 and 24.55, as seen in memory); a constant 11; dampings 1.
- A reach limit of 3.6 (1.8 when `fling` is set).
- **It starts the dynamic balancer itself** (unless told not to): max steps = maxSteps, a lean
  setting of -0.03 unless chickenArms, and a "crouching" amount of 0.5 when crouching.
- So the reaction is: the balancer keeps him on his feet and stepping (up to 6 steps), the shot
  behaviour bends and twists the upper body, turns the head, and brings a hand to the wound; when
  the balancer gives up, catchFall takes over (useCatchFallOnFall).

## The per-frame parts (not ported; listed so the work can be split)
Each part has a start, a per-frame update and an end, switched by conditions on the flags, the
"new hit" bit and a "has fallen" bit:
- a part that runs while falling / when told to skip balancing
- spine reaction (the pain bend and twist), the largest part
- head look (at the wound first, then at the shooter's position, by the timers above)
- reach for the wound, left and right arm (uses the arm IK and wrist IK that are already ported)
- chicken arms, fling (arms thrown), melee variants
- shock spin (a spin impulse; min / max / decay / lift above)
- a leg-wound collapse after timeBeforeCollapseWoundLeg
- a final part that runs every frame

About 20 KB of machine code in all. Port order: balancer driving the ragdoll first (the stagger is
the balancer), then spine reaction, head look, reach for wound.

## Open
- Which body-part numbers mean which part (bodyPart, and the int beside hitPoint).
- What the game's task sends for other weapons, moving peds and repeated hits (the session scan).
- `shotRelax`: settings and when the task sends it (taskparams: shotRelaxPeriod_Injured 1.5,
  FatallyInjured 0.5).

## Names settled (2 Oct)
The three settings RDR's list lacks are spinePainTime (hash 0x0a3f8806), spinePainMultiplier
(0x2953f83a) and spinePainTwistMultiplier (0x1a9ad24f): the names hash to exactly those values
(found by an external reviewer, re-checked here). Where the text above still says "hash ...", read these.
bodyPart is most likely the index of the hit part among the character's own 21 ragdoll parts (RDR
describes it as "index of part being hit"); to be checked against our part order with a known hit.

## Head look schedule (read 2 Oct, from the tick's structure; not yet run in the harness)
- State: one timer at shot object +0x178. Set on activate to headLookAtWoundMaxTimer (settings +0x6C = object +0x8C), so he looks at the wound first.
- Every tick, while the timer is >= 0: the timer counts down by the character's time step (character +0x350 -> +0x80) and the look target is the wound point at object +0xC0..+0xC8 (written earlier in the same tick, 0x83ed73-0x83eda1; how it's derived not read yet). When it goes below 0: the timer becomes minus a random value between headLookAtHeadPosMinTimer and MaxTimer (character's random generator at character +0x338, helper 0x617ba0), and a feedback object (character +0x348, slot +0x3C) is sent the name "shot" (vfn +0x14).
- While the timer is < 0 and the shot's useHeadLook flag (settings +0x74 bit 0x08) is set: the timer counts up and the target is the message's headLook point (settings +0x10, the shooter's head). When it passes 0: the timer becomes a random value between headLookAtWoundMinTimer and MaxTimer: back to the wound.
- With the real message values: wound 0.75 s first, then shooter 0.75-1.5 s, wound 0.1-0.75 s, and so on.
- The target is written into the headLook behaviour's settings: sub-behaviour table (shot object +0x10) slot 17 (+0x44), vtable 0xfe7ca0 (file address), target at +0x1C..+0x24, +0x3C = -1 (no instance). headLook's tick is vtable slot 5, 0x0084d940, over 5 KB. The table also holds: slot 0 the character (0xfe78fc), 1 the balancer (0xfe7c10), 5 the shot (0xfe7bc8); full list in behaviours.md.
