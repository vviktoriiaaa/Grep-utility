@echo off
setlocal
cd /d "%~dp0"
cargo run --locked
if errorlevel 1 pause
