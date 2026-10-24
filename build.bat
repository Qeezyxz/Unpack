@echo off
rem Build Unpack on Windows (needs: Rust from rustup.rs, Node.js LTS, and the WebView2 runtime which Windows 10/11 already has).
rem Output: src-tauri\target\release\unpack.exe and src-tauri\target\release\bundle\nsis\Unpack_*_x64-setup.exe
cd /d "%~dp0"
if not exist src-tauri\binaries\7za-x86_64-pc-windows-msvc.exe copy src-tauri\binaries\7za-x86_64-pc-windows-gnu.exe src-tauri\binaries\7za-x86_64-pc-windows-msvc.exe >nul
call npm install || goto :fail
call npx tauri build || goto :fail
echo.
echo Done. Installer is in src-tauri\target\release\bundle\nsis\
goto :eof
:fail
echo Build failed.
exit /b 1
