@echo off
setlocal enabledelayedexpansion

:: TigrimOS Installer for Windows
:: Clones, builds, and creates an installation folder

set APP_NAME=TigrimOS
set REPO_URL=https://github.com/Sompote/TigrimOSR.git
set BINARY_NAME=tigrimos.exe

echo.
echo ========================================
echo   TigrimOS Installer for Windows
echo ========================================
echo.

:: ── Check prerequisites ──
echo Checking prerequisites...

where git >nul 2>&1
if %errorlevel% neq 0 (
    echo [ERROR] git is not installed.
    echo   Install from: https://git-scm.com/download/win
    echo.
    pause
    exit /b 1
)

where rustc >nul 2>&1
if %errorlevel% neq 0 (
    echo [ERROR] Rust is not installed.
    echo   Install from: https://rustup.rs
    echo.
    pause
    exit /b 1
)

where cargo >nul 2>&1
if %errorlevel% neq 0 (
    echo [ERROR] cargo is not installed.
    echo   Install from: https://rustup.rs
    echo.
    pause
    exit /b 1
)

echo [OK] Prerequisites found (git, rustc, cargo)
echo.

:: ── Select install location ──
echo Where would you like to install TigrimOS?
echo.
echo   1) Home directory         (%USERPROFILE%\%APP_NAME%)
echo   2) Program Files          (%ProgramFiles%\%APP_NAME%)
echo   3) Custom location
echo.
set /p CHOICE="Select [1-3] (default: 1): "

if "%CHOICE%"=="2" (
    set "INSTALL_DIR=%ProgramFiles%\%APP_NAME%"
) else if "%CHOICE%"=="3" (
    set /p INSTALL_DIR="Enter full path: "
    if "!INSTALL_DIR!"=="" (
        echo [ERROR] No path provided. Aborting.
        pause
        exit /b 1
    )
) else (
    set "INSTALL_DIR=%USERPROFILE%\%APP_NAME%"
)

echo Install location: %INSTALL_DIR%
echo.

:: ── Clone or update repo ──
if exist "%INSTALL_DIR%\.git" (
    echo Existing installation found. Updating...
    cd /d "%INSTALL_DIR%"
    git pull --ff-only || echo [WARN] Pull failed, continuing with existing code...
) else (
    echo Cloning TigrimOS...
    if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"
    git clone %REPO_URL% "%INSTALL_DIR%"
    cd /d "%INSTALL_DIR%"
)

echo [OK] Source ready
echo.

:: ── Build ──
echo Building TigrimOS (release mode)...
echo This may take a few minutes on first build.
echo.

cargo build --release
if %errorlevel% neq 0 (
    echo.
    echo [ERROR] Build failed.
    pause
    exit /b 1
)

echo.
echo [OK] Build complete
echo.

:: ── Install Python data libraries (for search / charts / data analysis tools) ──
where python >nul 2>&1
if %errorlevel%==0 (
    echo Installing Python libraries ^(search, charts, data analysis^)...
    python -m pip install --upgrade pip >nul 2>&1
    python -m pip install duckduckgo-search matplotlib numpy pandas requests
    echo [OK] Python libraries installed
) else (
    echo [WARN] Python not found - skipping data libraries.
    echo        Install Python from https://www.python.org/downloads/ ^(check "Add to PATH"^),
    echo        then run: pip install duckduckgo-search matplotlib numpy pandas requests
)
echo.

:: ── Create installation folder ──
set "DIST_DIR=%INSTALL_DIR%\dist"
if exist "%DIST_DIR%" rmdir /s /q "%DIST_DIR%"
mkdir "%DIST_DIR%"

:: Copy binary
copy "%INSTALL_DIR%\target\release\%BINARY_NAME%" "%DIST_DIR%\%APP_NAME%.exe" >nul

:: Copy icon if exists
if exist "%INSTALL_DIR%\assets\icon.png" (
    copy "%INSTALL_DIR%\assets\icon.png" "%DIST_DIR%\icon.png" >nul
)

:: Copy data folder if exists
if exist "%INSTALL_DIR%\data" (
    xcopy "%INSTALL_DIR%\data" "%DIST_DIR%\data\" /e /i /q >nul 2>&1
)

echo [OK] Distribution created: %DIST_DIR%
echo.

:: ── Create desktop shortcut ──
set /p CREATE_SHORTCUT="Create desktop shortcut? [Y/n]: "
if /i "%CREATE_SHORTCUT%"=="n" goto :skip_shortcut

:: Use PowerShell to create shortcut
powershell -NoProfile -Command ^
    "$ws = New-Object -ComObject WScript.Shell; ^
     $sc = $ws.CreateShortcut('%USERPROFILE%\Desktop\%APP_NAME%.lnk'); ^
     $sc.TargetPath = '%DIST_DIR%\%APP_NAME%.exe'; ^
     $sc.WorkingDirectory = '%DIST_DIR%'; ^
     $sc.Description = 'TigrimOS - AI Agent Platform'; ^
     if (Test-Path '%DIST_DIR%\icon.png') { $sc.IconLocation = '%DIST_DIR%\%APP_NAME%.exe' }; ^
     $sc.Save()"

echo [OK] Desktop shortcut created
:skip_shortcut

:: ── Add to Start Menu ──
set /p CREATE_START="Add to Start Menu? [Y/n]: "
if /i "%CREATE_START%"=="n" goto :skip_start

set "START_DIR=%APPDATA%\Microsoft\Windows\Start Menu\Programs"
powershell -NoProfile -Command ^
    "$ws = New-Object -ComObject WScript.Shell; ^
     $sc = $ws.CreateShortcut('%START_DIR%\%APP_NAME%.lnk'); ^
     $sc.TargetPath = '%DIST_DIR%\%APP_NAME%.exe'; ^
     $sc.WorkingDirectory = '%DIST_DIR%'; ^
     $sc.Description = 'TigrimOS - AI Agent Platform'; ^
     $sc.Save()"

echo [OK] Start Menu entry created
:skip_start

:: ── Summary ──
echo.
echo ========================================
echo   Installation complete!
echo ========================================
echo.
echo   Source:    %INSTALL_DIR%
echo   App:      %DIST_DIR%\%APP_NAME%.exe
echo.

set /p LAUNCH="Launch %APP_NAME% now? [Y/n]: "
if /i not "%LAUNCH%"=="n" (
    start "" "%DIST_DIR%\%APP_NAME%.exe"
)

echo.
pause
