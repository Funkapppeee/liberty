# How the code is kept clean

The aim is for Liberty to behave like the original game, as closely as possible, **without any of
the original code entering this repository**.

## The rules

- **Never leaked source.** Nobody working on this opens leaked Rockstar or NaturalMotion source
  code or tools. Reading it would taint everything written afterwards.
- **Facts, not code.** What crosses from studying the game into this repository is behaviour and
  numbers: file layouts, constants, formulas, states and what triggers them. Never decompiled
  output, pseudo-code, or a function-by-function transliteration. The notes in `specs/` are
  written to that rule, and the Rust is written from those notes in its own structure.
- **Exactness comes from measurements.** Where a behaviour has to match exactly, the original is
  measured (inputs and the outputs they produce, numbers only) and the new code is tested against
  those numbers. The measurements come from the game, so they are not in this repository.
- **Nothing protected is unlocked.** A few routines in the game's executable are encrypted on disk
  by its copy protection. They are left alone; generic stand-ins are used instead.
- **The running game is only watched.** Tools that observe the real game read its memory and
  never write to it or inject anything.
- **Game data stays on the user's machine.** Anything extracted from the game lives in an ignored
  folder and is never committed.

## References

Public documentation of the file formats by the modding community (for example the GTAMods wiki
and open-source tools for later games in the series) was used for facts about formats. No code
was copied from those projects.
