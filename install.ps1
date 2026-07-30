# AndrewOS Installer for Windows (PowerShell)
# Clones, builds, and creates an installation folder.
#
# Designed to be run directly via:
#   irm https://raw.githubusercontent.com/Sompote/AndrewOS/main/install.ps1 | iex
#
# Optional environment overrides (useful for non-interactive / piped runs):
#   $env:ANDREWOS_INSTALL_DIR   -> install location (skips the location prompt)
#   $env:ANDREWOS_NONINTERACTIVE = "1" -> accept all defaults, no prompts

$APP_NAME    = "AndrewOS"
$REPO_URL    = "https://github.com/Sompote/AndrewOS.git"
$BINARY_NAME = "andrewos.exe"

# ── Helpers ──
function Test-NonInteractive {
    return ($env:ANDREWOS_NONINTERACTIVE -eq "1")
}

function Read-Default {
    param([string]$Prompt, [string]$Default = "")
    if (Test-NonInteractive) { return $Default }
    $answer = Read-Host $Prompt
    if ([string]::IsNullOrWhiteSpace($answer)) { return $Default }
    return $answer
}

function Add-ToSessionPath {
    param([string]$Dir)
    if ((Test-Path $Dir) -and ($env:Path -notlike "*$Dir*")) {
        $env:Path = "$Dir;$env:Path"
    }
}

# Reload PATH from the registry (Machine + User) so tools just installed via
# winget become visible in THIS session — otherwise you'd need a new terminal,
# which is the #1 reason the piped installer "can't find git/cargo/python".
function Update-SessionPath {
    try {
        $machine = [System.Environment]::GetEnvironmentVariable("Path", "Machine")
        $user    = [System.Environment]::GetEnvironmentVariable("Path", "User")
        $merged  = (@($machine, $user) | Where-Object { $_ }) -join ";"
        if ($merged) { $env:Path = "$merged;$env:Path" }
    } catch { }
    Add-ToSessionPath "$env:USERPROFILE\.cargo\bin"
}

# ── Ensure a tool exists, installing it via winget if missing ──
function Ensure-Tool {
    param(
        [string]$Command,         # command to test for (e.g. "git")
        [string]$WingetId,        # winget package id
        [string]$FriendlyName
    )

    if (Get-Command $Command -ErrorAction SilentlyContinue) {
        Write-Host "[OK] $FriendlyName found" -ForegroundColor Green
        return $true
    }

    # winget writes progress to stderr; don't let that abort the script.
    $ErrorActionPreference = "Continue"
    Write-Host "[..] $FriendlyName not found - attempting install..." -ForegroundColor Yellow
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        try {
            # Out-Host so winget's output does not pollute this function's return value.
            winget install --id $WingetId -e --silent `
                --accept-source-agreements --accept-package-agreements | Out-Host
        } catch {
            Write-Host "[WARN] winget install of $FriendlyName failed: $_" -ForegroundColor Yellow
        }
    } else {
        Write-Host "[WARN] winget is not available to auto-install $FriendlyName." -ForegroundColor Yellow
    }

    return $false
}

# ── Ensure Rust (rustc + cargo) ──
function Ensure-Rust {
    # winget / rustup-init write progress to stderr; don't treat that as fatal.
    $ErrorActionPreference = "Continue"

    $cargoBin = "$env:USERPROFILE\.cargo\bin"
    Add-ToSessionPath $cargoBin

    if ((Get-Command cargo -ErrorAction SilentlyContinue) -and
        (Get-Command rustc -ErrorAction SilentlyContinue)) {
        Write-Host "[OK] Rust toolchain found (rustc, cargo)" -ForegroundColor Green
        return $true
    }

    Write-Host "[..] Rust not found - installing via rustup..." -ForegroundColor Yellow

    # Prefer winget (installs rustup), fall back to rustup-init.exe download.
    $installed = $false
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        try {
            winget install --id Rustlang.Rustup -e --silent `
                --accept-source-agreements --accept-package-agreements | Out-Host
            $installed = $true
        } catch {
            Write-Host "[WARN] winget install of Rust failed, trying rustup-init..." -ForegroundColor Yellow
        }
    }

    if (-not $installed) {
        try {
            $rustupInit = Join-Path $env:TEMP "rustup-init.exe"
            Write-Host "Downloading rustup-init.exe..." -ForegroundColor Cyan
            Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustupInit -UseBasicParsing -ErrorAction Stop
            & $rustupInit -y --default-toolchain stable --profile minimal | Out-Host
        } catch {
            Write-Host "[ERROR] Automatic Rust install failed: $_" -ForegroundColor Red
            Write-Host "        Install Rust manually from https://rustup.rs then re-run." -ForegroundColor Yellow
            return $false
        }
    }

    # rustup installs to ~/.cargo/bin; make it visible in THIS session.
    Update-SessionPath
    Add-ToSessionPath $cargoBin

    # winget's Rustup package sometimes installs rustup WITHOUT a default
    # toolchain, leaving `cargo build` to fail with "no default toolchain".
    # Force-install the MSVC stable toolchain so the build can proceed.
    if (Get-Command rustup -ErrorAction SilentlyContinue) {
        try {
            rustup toolchain install stable-x86_64-pc-windows-msvc --no-self-update | Out-Host
            rustup default stable-x86_64-pc-windows-msvc | Out-Host
        } catch {
            Write-Host "[WARN] Could not set default Rust toolchain: $_" -ForegroundColor Yellow
        }
        Update-SessionPath
    }

    if ((Get-Command cargo -ErrorAction SilentlyContinue) -and
        (Get-Command rustc -ErrorAction SilentlyContinue)) {
        Write-Host "[OK] Rust toolchain installed" -ForegroundColor Green
        return $true
    }

    Write-Host "[ERROR] Rust was installed but cargo is not on PATH yet." -ForegroundColor Red
    Write-Host "        Open a NEW terminal and re-run the installer." -ForegroundColor Yellow
    return $false
}

# ── Ensure MSVC C++ build tools (Rust's default Windows linker) ──
function Test-MsvcTools {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $path = & $vswhere -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath
        if ($path) { return $true }
    }
    # Fallback: link.exe reachable (e.g. inside a Developer prompt).
    return [bool](Get-Command link.exe -ErrorAction SilentlyContinue)
}

function Test-IsAdmin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltinRole]::Administrator)
}

function Ensure-BuildTools {
    # The msvc Rust toolchain needs the MSVC linker (link.exe) from the
    # Visual Studio C++ Build Tools. Without it cargo fails: "link.exe not found".
    # Installing Build Tools requires Administrator rights, so we download the
    # official bootstrapper and launch it elevated (a UAC prompt appears).
    $ErrorActionPreference = "Continue"

    if (Test-MsvcTools) {
        Write-Host "[OK] MSVC C++ build tools found" -ForegroundColor Green
        return $true
    }

    Write-Host "[..] MSVC C++ build tools not found." -ForegroundColor Yellow
    Write-Host "     Installing Visual Studio 2022 Build Tools (C++ workload)." -ForegroundColor Yellow
    Write-Host "     Large download (~2-7 GB); needs Administrator approval (UAC)." -ForegroundColor DarkYellow

    $vsExe = Join-Path $env:TEMP "vs_BuildTools.exe"
    try {
        Write-Host "     Downloading VS Build Tools bootstrapper..." -ForegroundColor Cyan
        Invoke-WebRequest -Uri "https://aka.ms/vs/17/release/vs_BuildTools.exe" `
            -OutFile $vsExe -UseBasicParsing -ErrorAction Stop
    } catch {
        Write-Host "[ERROR] Could not download VS Build Tools installer: $_" -ForegroundColor Red
        return $false
    }

    $vsArgs = @(
        "--quiet", "--wait", "--norestart", "--nocache",
        "--add", "Microsoft.VisualStudio.Workload.VCTools",
        "--add", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
        "--includeRecommended"
    )

    try {
        Write-Host "     Launching installer (approve the UAC prompt)..." -ForegroundColor Cyan
        # -Verb RunAs elevates; -Wait blocks until the install finishes.
        $verb = if (Test-IsAdmin) { @{} } else { @{ Verb = "RunAs" } }
        $proc = Start-Process -FilePath $vsExe -ArgumentList $vsArgs -Wait -PassThru @verb
        Write-Host "     Installer exit code: $($proc.ExitCode)" -ForegroundColor DarkGray
    } catch {
        Write-Host "[ERROR] Elevated install was cancelled or failed: $_" -ForegroundColor Red
        Write-Host "        Re-run from an elevated terminal, or install" -ForegroundColor Yellow
        Write-Host "        'Desktop development with C++' (VS Build Tools 2022) manually." -ForegroundColor Yellow
        return $false
    }

    if (Test-MsvcTools) {
        Write-Host "[OK] MSVC C++ build tools installed" -ForegroundColor Green
        return $true
    }

    Write-Host "[ERROR] MSVC build tools still not detected after install." -ForegroundColor Red
    Write-Host "        A reboot may be required; then re-run the installer." -ForegroundColor Yellow
    return $false
}

# ── Ensure Python + the data libraries the app's tools rely on ──
# (web search = duckduckgo-search, charts = matplotlib, data = numpy/pandas).
# Optional: the app builds and runs without it, but Python-backed tools won't
# work until these are installed. This is non-fatal.
function Ensure-Python {
    $ErrorActionPreference = "Continue"

    $want = Read-Default "Install Python + data libraries (search, charts, data analysis)? [Y/n]" "Y"
    if ($want -eq "n") {
        Write-Host "[..] Skipping Python setup (run it later: pip install duckduckgo-search matplotlib numpy pandas requests)" -ForegroundColor DarkGray
        return
    }

    $hasPy = (Get-Command python -ErrorAction SilentlyContinue) -or (Get-Command py -ErrorAction SilentlyContinue)
    if (-not $hasPy) {
        Write-Host "[..] Python not found - installing Python 3.12..." -ForegroundColor Yellow
        if (Get-Command winget -ErrorAction SilentlyContinue) {
            try {
                winget install --id Python.Python.3.12 -e --silent `
                    --accept-source-agreements --accept-package-agreements | Out-Host
            } catch {
                Write-Host "[WARN] winget install of Python failed: $_" -ForegroundColor Yellow
            }
        } else {
            Write-Host "[WARN] winget unavailable; install Python from https://www.python.org/downloads/ (check 'Add to PATH')." -ForegroundColor Yellow
        }
        Update-SessionPath
    }

    # Resolve a usable interpreter (python, then the py launcher).
    $py = $null
    foreach ($cand in @("python", "py")) {
        if (Get-Command $cand -ErrorAction SilentlyContinue) { $py = $cand; break }
    }
    if (-not $py) {
        Write-Host "[WARN] Python is not on PATH yet. Open a new terminal and run:" -ForegroundColor Yellow
        Write-Host "       python -m pip install duckduckgo-search matplotlib numpy pandas requests" -ForegroundColor Yellow
        return
    }

    Write-Host "[..] Installing Python libraries (this can take a minute)..." -ForegroundColor Yellow
    try {
        & $py -m pip install --upgrade pip | Out-Host
        & $py -m pip install duckduckgo-search matplotlib numpy pandas requests | Out-Host
        if ($LASTEXITCODE -eq 0) {
            Write-Host "[OK] Python libraries installed" -ForegroundColor Green
        } else {
            Write-Host "[WARN] Some Python libraries may have failed to install (exit $LASTEXITCODE)." -ForegroundColor Yellow
        }
    } catch {
        Write-Host "[WARN] pip install failed: $_" -ForegroundColor Yellow
    }
}

# ── Prerequisites (auto-install where possible) ──
function Ensure-Prerequisites {
    Write-Host "Checking prerequisites..." -ForegroundColor Cyan

    Ensure-Tool -Command "git" -WingetId "Git.Git" -FriendlyName "git" | Out-Null
    Update-SessionPath
    Add-ToSessionPath "$env:ProgramFiles\Git\cmd"
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        Write-Host "[ERROR] git is required but could not be installed automatically." -ForegroundColor Red
        Write-Host "        Install it from https://git-scm.com/download/win then re-run." -ForegroundColor Yellow
        return $false
    }

    if (-not (Ensure-Rust))       { return $false }
    if (-not (Ensure-BuildTools)) { return $false }

    # Python is optional (the app still builds/runs), so failures are non-fatal.
    Ensure-Python

    Write-Host "[OK] All prerequisites ready" -ForegroundColor Green
    return $true
}

# ── Select location ──
function Select-Location {
    if ($env:ANDREWOS_INSTALL_DIR) { return $env:ANDREWOS_INSTALL_DIR }
    if (Test-NonInteractive)       { return "$env:USERPROFILE\$APP_NAME" }

    Write-Host ""
    Write-Host "Where would you like to install AndrewOS?" -ForegroundColor Yellow
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
                return $null
            }
            return $custom
        }
        default { return "$env:USERPROFILE\$APP_NAME" }
    }
}

# ── Clone or update ──
function Install-Source {
    param([string]$Dir)

    # git writes progress (cloning, pulling) to stderr; don't treat that as fatal.
    $ErrorActionPreference = "Continue"

    if (Test-Path "$Dir\.git") {
        Write-Host ""
        Write-Host "Existing installation found. Updating..." -ForegroundColor Yellow
        Push-Location $Dir
        try { git pull --ff-only }
        catch { Write-Host "[WARN] Pull failed, continuing with existing code..." -ForegroundColor Yellow }
    } else {
        Write-Host ""
        Write-Host "Cloning AndrewOS..." -ForegroundColor Cyan
        $parent = Split-Path $Dir -Parent
        if ($parent -and -not (Test-Path $parent)) {
            New-Item -ItemType Directory -Path $parent -Force | Out-Null
        }
        git clone $REPO_URL $Dir
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[ERROR] git clone failed." -ForegroundColor Red
            return $false
        }
        Push-Location $Dir
    }
    Write-Host "[OK] Source ready" -ForegroundColor Green
    return $true
}

# ── Build ──
function Build-App {
    # cargo writes normal progress to stderr; don't let that abort the script.
    $ErrorActionPreference = "Continue"

    Write-Host ""
    Write-Host "Building AndrewOS (release mode)..." -ForegroundColor Cyan
    Write-Host "This may take a few minutes on first build."
    Write-Host ""

    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[ERROR] Build failed." -ForegroundColor Red
        return $false
    }
    Write-Host ""
    Write-Host "[OK] Build complete" -ForegroundColor Green
    return $true
}

# ── Create dist folder ──
function Create-Distribution {
    param([string]$Dir)

    $distDir = "$Dir\dist"
    if (Test-Path $distDir) { Remove-Item $distDir -Recurse -Force }
    New-Item -ItemType Directory -Path $distDir -Force | Out-Null

    Copy-Item "$Dir\target\release\$BINARY_NAME" "$distDir\$APP_NAME.exe"

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

    if (Test-NonInteractive) { return }

    Write-Host ""
    $createDesktop = Read-Default "Create desktop shortcut? [Y/n]" "Y"
    if ($createDesktop -ne "n") {
        $ws = New-Object -ComObject WScript.Shell
        $sc = $ws.CreateShortcut("$env:USERPROFILE\Desktop\$APP_NAME.lnk")
        $sc.TargetPath = "$DistDir\$APP_NAME.exe"
        $sc.WorkingDirectory = $DistDir
        $sc.Description = "AndrewOS - AI Agent Platform"
        $sc.Save()
        Write-Host "[OK] Desktop shortcut created" -ForegroundColor Green
    }

    $createStart = Read-Default "Add to Start Menu? [Y/n]" "Y"
    if ($createStart -ne "n") {
        $startDir = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
        $ws = New-Object -ComObject WScript.Shell
        $sc = $ws.CreateShortcut("$startDir\$APP_NAME.lnk")
        $sc.TargetPath = "$DistDir\$APP_NAME.exe"
        $sc.WorkingDirectory = $DistDir
        $sc.Description = "AndrewOS - AI Agent Platform"
        $sc.Save()
        Write-Host "[OK] Start Menu entry created" -ForegroundColor Green
    }
}

# ── Main (wrapped so `return` aborts cleanly without killing the host) ──
function Install-AndrewOS {
    $ErrorActionPreference = "Stop"

    Write-Host ""
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "  AndrewOS Installer for Windows" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host ""

    if (-not (Ensure-Prerequisites)) { return }

    $installDir = Select-Location
    if (-not $installDir) { return }
    Write-Host "Install location: $installDir" -ForegroundColor Cyan

    try {
        if (-not (Install-Source -Dir $installDir)) { return }
        if (-not (Build-App)) { return }

        $distDir = Create-Distribution -Dir $installDir
        Create-Shortcuts -DistDir $distDir

        Write-Host ""
        Write-Host "========================================" -ForegroundColor Cyan
        Write-Host "  Installation complete!" -ForegroundColor Green
        Write-Host "========================================" -ForegroundColor Cyan
        Write-Host ""
        Write-Host "  Source:  $installDir"
        Write-Host "  App:     $distDir\$APP_NAME.exe"
        Write-Host ""

        $launch = Read-Default "Launch $APP_NAME now? [Y/n]" "n"
        if ($launch -ne "n") {
            Start-Process "$distDir\$APP_NAME.exe"
        }
    }
    finally {
        # Restore the original working directory regardless of outcome.
        if ((Get-Location -Stack).Count -gt 0) { Pop-Location }
    }
}

Install-AndrewOS
