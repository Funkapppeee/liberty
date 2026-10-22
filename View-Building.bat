@echo off
rem Opens the Liberty 3D viewer on 253 Broadway (GTA IV, Manhattan chunk 02).
rem Other models: View-Building.bat pc/data/maps/manhat/manhat02.img <name>.wdr
rem Controls: hold right mouse to look, WASD move, E/Q up/down, Shift fast, wheel speed, Esc quit.
start "" "target\release\liberty-view.exe" %*
