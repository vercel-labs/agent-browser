@echo off
setlocal
set "SCRIPT_DIR=%~dp0"
node "%SCRIPT_DIR%agent-browser" %*
exit /b %errorlevel%
