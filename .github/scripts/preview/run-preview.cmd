@echo off
setlocal
cd /d "%~dp0"
set EMBER_DEMO_DATA=1
if not "%~1"=="" set "EMBER_SERVER_URL=%~1"
"%~dp0ember-app.exe"
endlocal
