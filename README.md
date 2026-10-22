# Liberty

A game engine written from scratch in Rust (on [Bevy](https://bevyengine.org)) that loads **your own
copy** of Grand Theft Auto IV and plays it: the city, traffic, pedestrians, Niko on foot and in
cars, weapons, sound, time of day.

This repository contains **no game files**. No models, textures, sounds, scripts, executables or
encryption keys from the game are included, and none are needed to build it. To run it you need a
legitimately owned copy of GTA IV installed on your PC; Liberty reads that install, read-only, at
run time.

This is an unofficial fan project. It is not made by, endorsed by, or affiliated with Rockstar
Games or Take-Two Interactive. "Grand Theft Auto" and related names are their trademarks, used
here only to say what the engine is compatible with. See [NOTICE.md](NOTICE.md).

## State

Work in progress. What runs today:

- The whole map streams in from the game's archives, with collision, water and level-of-detail.
- Time of day and weather from the game's own table, including its colour grading and light levels.
- Niko on foot: the game's animations, jumping, a ragdoll, aiming, shooting, strafing, reloading.
- Cars: enter, drive, exit, with the game's handling data.
- Traffic on the game's road network: lanes, one-way streets, traffic lights, slowing for turns,
  waiting for crossing cars, reacting to gunfire.
- Pedestrians on the pavements: walking, fleeing, getting knocked down and getting back up.
- Sound from the game's banks: guns, footsteps, engines, horns, ambience.
- HUD: radar, weapon icon, ammunition.
- Street lamps from the lights stored in the game's models.

Not there yet: missions and scripts, interiors, police, the full Euphoria behaviours (in
progress, see [CLEANROOM.md](CLEANROOM.md)), saving, multiplayer.

## Downloads

Prebuilt Windows builds will be on this repository's [Releases](../../releases) page. The zip is
named `Liberty-<version>.zip` and has **no game files** inside: you still need your own copy of
GTA IV, installed on your PC. Unzip, point `LIBERTY_GAME` at the game folder (see below) and run
`liberty-view.exe`.

## Build and run

Windows, a recent stable Rust toolchain.

```
cargo build --release -p liberty-view
set LIBERTY_GAME=C:\Program Files (x86)\Steam\steamapps\common\Grand Theft Auto IV\GTAIV
target\release\liberty-view.exe --city city
```

`LIBERTY_GAME` is the folder that holds `GTAIV.exe`. Tested against the Steam Complete Edition
(1.2.0.59). `liberty-cli` (`cargo build --release -p liberty-cli`) is a set of developer tools for
looking inside the game's file formats.

## Controls and the dev panel

Interactive runs open a small dev panel (time, places, layers, settings, keys, and a console).

| Key | What |
|---|---|
| `` ` `` | console (type `help` for its commands) |
| `F1` | the full list of keys |
| `F2` | hide the panel and everything on screen |
| `F11` | fullscreen |
| `Esc` | close what is open; with nothing open, asks before quitting |

On foot: `W A S D` run, `Shift` sprint, `Space` jump, `F` get in or out of a car, right mouse or `Z`
aim, left mouse shoot, `R` reload, `1`-`9` pick a gun. In a car: `W`/`S` gas and brake, `A`/`D`
steer, `Space` handbrake, `H` horn. `T` jumps an hour, `Y` cycles the weather. `G` swaps between
walking and a free-flying camera.

## Layout

| Folder | What |
|---|---|
| `crates/rage` | readers for the game's file formats (archives, models, textures, collision, paths, navmesh, animation, audio, data tables) |
| `crates/liberty-view` | the game itself |
| `crates/liberty-cli` | developer tools |
| `crates/phys`, `crates/euphoria` | physics and character-behaviour work |
| `specs/` | behaviour notes the code is written from |

Some tests compare against measurements taken from the real game. Those measurements are not in
the repository (they come from the game), so those tests are skipped without them.

## How it was made

From the game's data files and from watching what the game does, not from anyone's source code.
See [CLEANROOM.md](CLEANROOM.md). Much of the code was written with AI assistance.

## Known issues

- Ragdolls and shot reactions do not look like the original game yet: the decision code follows
  what the game does, the body does not.
- No missions or scripts, interiors, police, or car damage.
- Traffic and pedestrians are simple compared with the original.
- It may crash on some PCs, or if the game folder is wrong. Loading the city takes a while.

## Licence

Licensed under either of the MIT licence or the Apache License 2.0, at your option. See
[LICENSE](LICENSE). This covers the code in this repository only, not the game.

## Contributing and feedback

Issues and pull requests are welcome. Contributions describe behaviour and numbers only: no
decompiled code and no game files, please. See [CLEANROOM.md](CLEANROOM.md).
