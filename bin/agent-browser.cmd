@echo off
setlocal

set "SCRIPT_DIR=%~dp0"

REM Detect architecture
if "%PROCESSOR_ARCHITECTURE%"=="AMD64" (
    set "ARCH=x64"
) else if "%PROCESSOR_ARCHITECTURE%"=="ARM64" (
    set "ARCH=arm64"
) else (
    set "ARCH=x64"
)

set "BINARY=%SCRIPT_DIR%agent-browser-win32-%ARCH%.exe"

if exist "%BINARY%" (
    "%BINARY%" %*
    exit /b %errorlevel%
)

echo Error: No binary found for win32-%ARCH% >&2
echo Run 'npm run build:native' to build for your platform >&2
exit /b 1
