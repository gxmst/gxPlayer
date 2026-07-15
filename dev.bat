@echo off
setlocal
REM GXPlayer dev mode - hot reload, use this for daily development
cd /d "%~dp0" || exit /b 1

REM Clean up GXPlayer itself so no old window lingers. Never kill an unrelated
REM process merely because it owns the configured Vite port.
taskkill /F /IM gxplayer.exe >nul 2>&1
for /f "tokens=5" %%p in ('netstat -ano ^| findstr ":1420" ^| findstr "LISTENING"') do (
    echo [GXPlayer] Port 1420 is already in use by PID %%p.
    echo [GXPlayer] Stop that process, or run the development command manually with a free port.
    pause
    endlocal & exit /b 1
)

echo [GXPlayer] Starting dev mode (npm run tauri dev)...
echo.
call npm run tauri dev
set "DEV_STATUS=%ERRORLEVEL%"
echo.
if not "%DEV_STATUS%"=="0" (
    echo [GXPlayer] Dev process failed with exit code %DEV_STATUS%.
) else (
    echo [GXPlayer] Dev process exited normally.
)
pause
endlocal & exit /b %DEV_STATUS%
