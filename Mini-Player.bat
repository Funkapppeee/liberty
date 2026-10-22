@echo off
rem Liberty mini player: sits beside the real GTA IV (start the game too). Never takes focus, no sound.
rem F9 / F8 step through the checklist. Episodes are saved to out\s1\live.
cd /d "%~dp0"
start "" "target-s1b\release\liberty-view.exe" --mini
