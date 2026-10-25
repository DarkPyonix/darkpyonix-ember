@echo off
setlocal
cd /d "%~dp0"
set EMBER_DEMO_DATA=1
set EMBER_SCRIPTED_AGENT=1
set EMBER_DATA_DIR=%LOCALAPPDATA%\EmberPreview
set EMBER_LISTEN=127.0.0.1:8740
set EMBER_SERVER_URL=http://127.0.0.1:8740
if not exist "%EMBER_DATA_DIR%" mkdir "%EMBER_DATA_DIR%"
start "ember-server" /min "%~dp0ember-server.exe"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0seed-demo.ps1" -Url "%EMBER_SERVER_URL%" -Dir "%EMBER_DATA_DIR%\demo-project"
"%~dp0ember-app.exe"
taskkill /im ember-server.exe /f >nul 2>&1
endlocal
