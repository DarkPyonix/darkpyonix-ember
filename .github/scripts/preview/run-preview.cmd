@echo off
setlocal
cd /d "%~dp0"
set EMBER_DEMO_DATA=1
set "EMBER_SERVER_URL=%~1"
if "%EMBER_SERVER_URL%"=="" set /p EMBER_SERVER_URL=Server URL [http://127.0.0.1:8740]: 
if "%EMBER_SERVER_URL%"=="" set EMBER_SERVER_URL=http://127.0.0.1:8740
"%~dp0ember-app.exe"
endlocal
