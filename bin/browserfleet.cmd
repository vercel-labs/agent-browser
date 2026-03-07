@echo off
setlocal
set "SCRIPT_DIR=%~dp0"
node "%SCRIPT_DIR%..\dist\index.js" %*
exit /b %errorlevel%
