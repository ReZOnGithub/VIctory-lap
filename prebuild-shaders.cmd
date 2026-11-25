@echo off
rem Translates every shader the game has used so far (the library the layer
rem keeps in %LOCALAPPDATA%\victory\cache\library) into the shader cache, on
rem all cores, without starting the game. Run it after building a new
rem victory.exe whose translator changed, before playing: the game then finds
rem every shader translated instead of translating each at its first draw.
"%~dp0target\release\victory.exe" translate --warm all "%LOCALAPPDATA%\victory\cache\library"
pause
