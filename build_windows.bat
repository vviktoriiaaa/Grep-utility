@echo off
setlocal
cd /d "%~dp0"
where cargo >nul 2>nul
if errorlevel 1 (
 echo Rust/Cargo not found. Install Rust from https://rustup.rs then reopen this file.
 pause
 exit /b 1
)
cargo build --release --locked
if errorlevel 1 (
 echo Build failed. Check Rust and Visual Studio C++ Build Tools with Windows SDK.
 pause
 exit /b 1
)
if not exist dist mkdir dist
copy /y "target\release\mygrep.exe" "dist\mygrep.exe" >nul
echo Ready: dist\mygrep.exe
pause
