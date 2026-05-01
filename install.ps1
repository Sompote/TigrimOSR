# TigrimOS Installer for Windows (PowerShell)
# Clones, builds, and creates an installation folder

$ErrorActionPreference = "Stop"

$APP_NAME = "TigrimOS"
$REPO_URL = "https://github.com/Sompote/TigrimOSR.git"
$BINARY_NAME = "tigrimos.exe"

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  TigrimOS Installer for Windows" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""

# ── Check prerequisites ──
function Test-Prerequisites {
    $missing = @()
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { $missing += "git" }
    if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) { $missing += "rustc" }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $missing += "cargo" }

    if ($missing.Count -gt 0) {
        Write-Host "[ERROR] Missing: $($missing -join ', ')" -ForegroundColor Red
        Write-Host ""
        if ($missing -contains "rustc" -or $missing -contains "cargo") {
            Write-Host "  Install Rust: https://rustup.rs" -ForegroundColor Yellow
        }
        if ($missing -contains "git") {
            Write-Host "  Install git:  https://git-scm.com/download/win" -ForegroundColor Yellow
        }
        exit 1
    }
    Write-Host "[OK] Prerequisites found (git, rustc, cargo)" -ForegroundColor Green
}

# ── Select location ──
function Select-Location {
    Write-Host ""
    Write-Host "Where would you like to install TigrimOS?" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  1) Home directory         ($env:USERPROFILE\$APP_NAME)"
    Write-Host "  2) Program Files          ($env:ProgramFiles\$APP_NAME)"
    Write-Host "  3) Custom location"
    Write-Host ""

    $choice = Read-Host "Select [1-3] (default: 1)"

    switch ($choice) {
        "2" { return "$env:ProgramFiles\$APP_NAME" }
        "3" {
            $custom = Read-Host "Enter full path"
            if ([string]::IsNullOrWhiteSpace($custom)) {
                Write-Host "[ERROR] No path provided." -ForegroundColor Red
                exit 1
            }
            return $custom
        }
        default { return "$env:USERPROFILE\$APP_NAME" }
    }
}

# ── Clone or update ──
function Install-Source {
    param([string]$Dir)

    if (Test-Path "$Dir\.git") {
        Write-Host ""
        Write-Host "Existing installation found. Updating..." -ForegroundColor Yellow
        Push-Location $Dir
        try { git pull --ff-only }
        catch { Write-Host "[WARN] Pull failed, continuing with existing code..." -ForegroundColor Yellow }
    } else {
        Write-Host ""
        Write-Host "Cloning TigrimOS..." -ForegroundColor Cyan
        $parent = Split-Path $Dir -Parent
        if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
        git clone $REPO_URL $Dir
        Push-Location $Dir
    }
    Write-Host "[OK] Source ready" -ForegroundColor Green
}

# ── Build ──
function Build-App {
    Write-Host ""
    Write-Host "Building TigrimOS (release mode)..." -ForegroundColor Cyan
    Write-Host "This may take a few minutes on first build."
    Write-Host ""

    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[ERROR] Build failed." -ForegroundColor Red
        exit 1
    }
    Write-Host ""
    Write-Host "[OK] Build complete" -ForegroundColor Green
}

# ── Create dist folder ──
function Create-Distribution {
    param([string]$Dir)

    $distDir = "$Dir\dist"
    if (Test-Path $distDir) { Remove-Item $distDir -Recurse -Force }
    New-Item -ItemType Directory -Path $distDir -Force | Out-Null

    # Copy binary
    Copy-Item "$Dir\target\release\$BINARY_NAME" "$distDir\$APP_NAME.exe"

    # Copy icon
    if (Test-Path "$Dir\assets\icon.png") {
        Copy-Item "$Dir\assets\icon.png" "$distDir\icon.png"
    }

    Write-Host ""
    Write-Host "[OK] Distribution created: $distDir" -ForegroundColor Green

    return $distDir
}

# ── Create shortcuts ──
function Create-Shortcuts {
    param([string]$DistDir)

    # Desktop shortcut
    Write-Host ""
    $createDesktop = Read-Host "Create desktop shortcut? [Y/n]"
    if ($createDesktop -ne "n") {
        $ws = New-Object -ComObject WScript.Shell
        $sc = $ws.CreateShortcut("$env:USERPROFILE\Desktop\$APP_NAME.lnk")
        $sc.TargetPath = "$DistDir\$APP_NAME.exe"
        $sc.WorkingDirectory = $DistDir
        $sc.Description = "TigrimOS - AI Agent Platform"
        $sc.Save()
        Write-Host "[OK] Desktop shortcut created" -ForegroundColor Green
    }

    # Start Menu
    $createStart = Read-Host "Add to Start Menu? [Y/n]"
    if ($createStart -ne "n") {
        $startDir = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
        $ws = New-Object -ComObject WScript.Shell
        $sc = $ws.CreateShortcut("$startDir\$APP_NAME.lnk")
        $sc.TargetPath = "$DistDir\$APP_NAME.exe"
        $sc.WorkingDirectory = $DistDir
        $sc.Description = "TigrimOS - AI Agent Platform"
        $sc.Save()
        Write-Host "[OK] Start Menu entry created" -ForegroundColor Green
    }
}

# ── Run ──
Test-Prerequisites
$installDir = Select-Location
Write-Host "Install location: $installDir" -ForegroundColor Cyan
Install-Source -Dir $installDir
Build-App
$distDir = Create-Distribution -Dir $installDir
Create-Shortcuts -DistDir $distDir

Pop-Location

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  Installation complete!" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Source:  $installDir"
Write-Host "  App:     $distDir\$APP_NAME.exe"
Write-Host ""

$launch = Read-Host "Launch $APP_NAME now? [Y/n]"
if ($launch -ne "n") {
    Start-Process "$distDir\$APP_NAME.exe"
}
