@echo off
setlocal

:: Detect architecture
set "ARCH=x64"
if "%PROCESSOR_ARCHITECTURE%"=="ARM64" set "ARCH=arm64"
if "%PROCESSOR_ARCHITECTURE%"=="x86" (
    if not defined PROCESSOR_ARCHITEW6432 set "ARCH=x86"
)

:: Get script directory
set "SCRIPT_DIR=%~dp0"

:: Path to the Windows binary
set "BINARY=%SCRIPT_DIR%agent-browser-win32-%ARCH%.exe"

:: Check if binary exists
if not exist "%BINARY%" (
    echo Error: agent-browser binary not found at %BINARY% >&2
    echo Run 'npm install' to download the binary, or 'npm run build:native' to build it locally. >&2
    exit /b 1
)

:: Run the binary with all arguments
"%BINARY%" %*
exit /b %errorlevel%
