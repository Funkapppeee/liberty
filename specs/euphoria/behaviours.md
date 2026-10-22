# GTA IV's Euphoria behaviours: the full list (reader side, 2 Oct 2026)

What IV actually ships, from the game's own class names (RTTI, `out/ghidra/decomp/_index.txt`) and
its message dispatcher (0x00827350, `re/nm_messages.py` -> `out/nm_messages.txt`). IV has **19
behaviour classes** (`NmRsCBU*`) and **31 messages**; a message starts or configures a behaviour.
Each character holds one object of every class in a table (shot object +0x10 points at it; slot
order below). Decompiled vtable routines per class: `out/ghidra/decomp/ART_Rockstar_NmRsCBU<Name>/`
(`005_*.c` = the per-frame tick). "Lines" = the tick's decompiled lines / all its vtable routines;
helpers they call are extra (the shot's tick calls 33).

| Slot | Class | vtable | Tick | Lines | Port status |
|---|---|---|---|---|---|
| 1 | DynamicBalancer | 0xfe7c10 | 0x844290 | 28 / 145 (+ the four big parts) | **ported**, matches the game (foot placement, pelvis, legs, fall check); drives our ragdoll |
| 2 | BodyBalance | 0xfe7de4 | 0x858a80 | 1311 / 1555 | **ported**, matches (27,500 values / 500 cases; bodyBalance.md); drives our ragdoll's arms, spine and head on the balancer. Head look and body turn choices not ported |
| 3 | BraceForImpact | 0xfe7bec | 0x840820 | 46 / 102 (+ parts) | **ported**, matches (parts 1, 2a, 2b) |
| 4 | BodyFoetal | 0xfe7d54 | 0x852830 | 134 / 314 | not ported |
| 5 | Shot | 0xfe7bc8 | 0x83e910 | 576 / 644 (+ ~14 parts) | stagger on the balancer, the 0.4 s loosening, catchFall on fall; head-look schedule read (shot.md); reach for wound ported (reachForWound.md, matches the game); spine pain, shock spin, fling, chicken arms, leg collapse not ported |
| 6 | SpineTwist | 0xfe7d0c | 0x8515a0 | 136 / 254 | not ported |
| 7 | CatchFall | 0xfe7ba4 | 0x83c940 | 311 / 338 | **ported**, matches (800/800 frames) |
| 8 | RollUp | 0xfe7ce8 | 0x84fc80 | 330 / 487 | not ported (bodyRollUp) |
| 9 | Flinch | 0xfe7c7c | 0x84b360 | 89 / 222 | not ported (upperBodyFlinch) |
| 10 | FallOverWall | 0xfe7e08 | 0x85b3c0 | 502 / 570 | not ported |
| 11 | HighFall | 0xfe7d9c | 0x854550 | 104 / 223 | not ported |
| 12 | RollDownStairs | 0xfe7cc4 | 0x84f430 | 80 / 249 | stand-in only (a tucked "Roll" mode) |
| 13 | BodyWrithe | 0xfe7d78 | 0x8530f0 | 43 / 185 | not ported |
| 14 | Pedal | 0xfe7dc0 | 0x857cb0 | 181 / 330 | not ported (pedalLegs) |
| 15 | ArmsWindmillAdaptive | 0xfe7d30 | 0x851cc0 | 173 / 266 | not ported |
| 16 | Grab | 0xfe7c58 | 0x84a2b0 | 381 / 556 | not ported |
| 17 | HeadLook | 0xfe7ca0 | 0x84d940 | 432 / 529 | **ported**, matches IV on 400/400 random cases (crates/euphoria::headlook); drives shot peds' heads |
| 18 | PointArm | 0xfe7b5c | 0x83bc30 | 197 / 259 | not ported |
| 0 | (the character itself, NmRsCharacter 0xfe78fc) | | | | |

Also ported, used by several behaviours: arm IK (320/320), wrist IK, the joint muscle model and
gravity opposition (balancer.md).

## The queue (S2 behaviour factory, from 7 Oct; top first)
Recipe per item: `specs/euphoria/FACTORY.md` (harness on `re/nmharness.py`, real settings via
`re/nm_params.py`, port + vector test + 5 break checks + spec with a wiring note for S1).
| # | Item | Where | Status |
|---|---|---|---|
| 1 | Shot's own parts: spine pain (start 0x860d20 / update 0x860fa0 / end 0x8619c0, plus the tick's spine loosening), chicken arms (0x8606d0 / 0x860740), injured arm (0x863a20, per side), wounded-leg collapse (0x863b20, per side), shock spin (in the tick), the part gating | shot tick 0x83e910 | S2 chat, in progress |
| 2 | HighFall | slot 11 | queued |
| 3 | RollDownStairs | slot 12 | queued |
| 4 | BodyWrithe | slot 13 | queued |
| 5 | ArmsWindmillAdaptive + Pedal | slots 15, 14 | queued |
| 6 | FallOverWall | slot 10 | queued |
| 7 | Flinch | slot 9 | queued |
| 8 | RollUp | slot 8 | queued |
| 9 | BodyFoetal | slot 4 | queued |
| 10 | Grab (a ped holding a car door: session minute 68) | slot 16 | queued |
| 11 | PointArm | slot 18 | queued |
| 12 | SpineTwist | slot 6 | queued |
| 13 | bodyBalance's head look and body turn | slot 2 | queued |
| - | Shot leftovers: fling (0x85f3c0 / 0x85f780 / 0x8602c0), crouch (0x83fd50), falling part (0x860890 / 0x860980), the arms part 0x862ec0 / 0x862fc0, melee (0x85e9e0 / 0x85eac0) | shot | after 1 |

## The 31 messages IV's dispatcher handles
Behaviours: activePose, applyImpulse, armsWindmillAdaptive, bodyBalance, bodyFoetal, bodyRelax,
bodyRollUp, bodyTwist, bodyWrithe, braceForImpact, catchFall, fallOverWall, grab, headLook,
highFall, leanInDirection, leanToPosition, leanTowardsObject, pedalLegs, pointArm, rollDownStairs,
shot, shotRelax, stayUpright, upperBodyFlinch. Settings: configureBalance, defineAttachedObject,
forceToBodyPart, incomingTransforms, setStiffness, stopAllBehaviours. (Some messages share a class:
bodyRelax / bodyTwist / lean* / stayUpright / activePose / applyImpulse are handled by the character
or BodyBalance; to be mapped from each case's activate call.)

## What each is for (from the names and NaturalMotion's settings lists; confirm when read)
- **BodyBalance**: the whole standing reaction around the balancer: arms out on a push, spine and
  head while staggering, when to give up. The biggest one, and what a shoved or shot ped looks like.
- **HeadLook**: turns neck and head toward a point (the shot's wound / shooter; catchFall's ground).
- **HighFall**: falling from a height: arms and legs brace for landing.
- **FallOverWall**: being pushed over a railing or low wall.
- **RollDownStairs / RollUp**: tumbling (stairs, slopes, after car hits).
- **BodyWrithe / BodyFoetal**: on the ground hurt: writhing, curling up.
- **Pedal / ArmsWindmillAdaptive**: legs pedalling and arms windmilling while airborne.
- **Grab**: grabbing a ledge, a car door, a person.
- **Flinch**: upper body flinching away (explosions, near misses, a car close by).
- **SpineTwist, PointArm**: twist the spine / point an arm at something (used by others).

## Reading order (proposal)
1. HeadLook (432 lines): the shot and catchFall both use it.
2. The shot's remaining parts: spine pain, reach for wound (arm IK ready), leg collapse, shock spin.
3. BodyBalance (1311 lines): arms and spine on the balancer, so shoves and shots look like IV.
4. The falls: HighFall, RollDownStairs, BodyWrithe, ArmsWindmillAdaptive, Pedal.
5. FallOverWall, RollUp, Flinch, BodyFoetal, Grab, PointArm, SpineTwist.
Method for each: read the structure (decompile + asm), run IV's own routine in the harness on test
inputs, port, replay every case, break the port on purpose (match-original skill), then compare
with the recording's real characters where it was used.
