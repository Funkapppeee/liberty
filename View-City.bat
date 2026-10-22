@echo off
rem Opens the Liberty 3D viewer, streaming GTA IV's Liberty City from your own install.
rem Default: the whole city, starting above the tennis courts in Meadows Park (open ground). Detail streams in around the camera.
rem Just some chunks:  View-Chunk.bat manhat*      or   View-Chunk.bat manhat02,manhat03
rem Controls: hold right mouse to look, WASD move, E/Q up/down, Shift fast, wheel speed,
rem           + / - view distance, Esc quit.
set CHUNKS=%1
if "%CHUNKS%"=="" set CHUNKS=city
start "" "target\release\liberty-view.exe" --city %CHUNKS%
