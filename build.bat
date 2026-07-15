@echo off
setlocal
REM GXPlayer installer build. Cargo/Tauri selects the configured output directory.
cd /d "%~dp0" || exit /b 1
echo [GXPlayer] Closing any running gxplayer.exe...
taskkill /F /IM gxplayer.exe >nul 2>&1
echo [GXPlayer] Building app (npm run tauri build)...
echo.
call npm run tauri build
set "BUILD_STATUS=%ERRORLEVEL%"
echo.
if not "%BUILD_STATUS%"=="0" (
    echo [GXPlayer] Build failed with exit code %BUILD_STATUS%.
    pause
    endlocal & exit /b %BUILD_STATUS%
)
echo [GXPlayer] Build finished. Tauri printed the exact executable and installer paths above.
pause
endlocal & exit /b 0
