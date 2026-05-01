#!/bin/bash
# TigrimOS Installer for macOS
# Clones, builds, and creates a .app bundle

set -e

APP_NAME="TigrimOS"
REPO_URL="https://github.com/Sompote/TigrimOSR.git"
BINARY_NAME="tigrimos"

# ── Colors ──
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

echo ""
echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}  TigrimOS Installer for macOS${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""

# ── Check prerequisites ──
check_prereqs() {
    local missing=()
    if ! command -v git &>/dev/null; then missing+=("git"); fi
    if ! command -v rustc &>/dev/null; then missing+=("rustc"); fi
    if ! command -v cargo &>/dev/null; then missing+=("cargo"); fi

    if [ ${#missing[@]} -gt 0 ]; then
        echo -e "${RED}Missing required tools: ${missing[*]}${NC}"
        echo ""
        if [[ " ${missing[*]} " =~ " rustc " ]] || [[ " ${missing[*]} " =~ " cargo " ]]; then
            echo "Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        fi
        if [[ " ${missing[*]} " =~ " git " ]]; then
            echo "Install git:  xcode-select --install"
        fi
        exit 1
    fi
    echo -e "${GREEN}[OK]${NC} Prerequisites found (git, rustc, cargo)"
}

# ── Select clone location ──
select_location() {
    echo ""
    echo -e "${YELLOW}Where would you like to install TigrimOS?${NC}"
    echo ""
    echo "  1) Home directory       (~/$APP_NAME)"
    echo "  2) Applications Support (~/Library/Application Support/$APP_NAME)"
    echo "  3) Custom location"
    echo ""
    read -rp "Select [1-3] (default: 1): " choice </dev/tty

    case "$choice" in
        2) INSTALL_DIR="$HOME/Library/Application Support/$APP_NAME" ;;
        3)
            read -rp "Enter full path: " custom_path </dev/tty
            if [ -z "$custom_path" ]; then
                echo -e "${RED}No path provided. Aborting.${NC}"
                exit 1
            fi
            INSTALL_DIR="$custom_path"
            ;;
        *) INSTALL_DIR="$HOME/$APP_NAME" ;;
    esac

    echo -e "${BLUE}Install location:${NC} $INSTALL_DIR"
}

# ── Clone or update repo ──
clone_repo() {
    if [ -d "$INSTALL_DIR/.git" ]; then
        echo ""
        echo -e "${YELLOW}Existing installation found. Updating...${NC}"
        cd "$INSTALL_DIR"
        git pull --ff-only || {
            echo -e "${YELLOW}Pull failed, continuing with existing code...${NC}"
        }
    else
        echo ""
        echo -e "${BLUE}Cloning TigrimOS...${NC}"
        # Remove existing non-git directory if present
        if [ -d "$INSTALL_DIR" ]; then
            echo -e "${YELLOW}Removing existing non-git directory...${NC}"
            rm -rf "$INSTALL_DIR"
        fi
        mkdir -p "$(dirname "$INSTALL_DIR")"
        git clone "$REPO_URL" "$INSTALL_DIR"
        cd "$INSTALL_DIR"
    fi
    echo -e "${GREEN}[OK]${NC} Source ready"
}

# ── Build ──
build_app() {
    cd "$INSTALL_DIR"
    echo ""
    echo -e "${BLUE}Building TigrimOS (release mode)...${NC}"
    echo "This may take a few minutes on first build."
    echo ""
    cargo build --release 2>&1 | tail -5
    echo ""
    echo -e "${GREEN}[OK]${NC} Build complete"
}

# ── Create macOS .app bundle ──
create_app_bundle() {
    local APP_DIR="$INSTALL_DIR/target/$APP_NAME.app"
    local CONTENTS="$APP_DIR/Contents"
    local MACOS_DIR="$CONTENTS/MacOS"
    local RESOURCES="$CONTENTS/Resources"

    echo ""
    echo -e "${BLUE}Creating $APP_NAME.app bundle...${NC}"

    rm -rf "$APP_DIR"
    mkdir -p "$MACOS_DIR" "$RESOURCES"

    # Copy binary
    cp "$INSTALL_DIR/target/release/$BINARY_NAME" "$MACOS_DIR/$APP_NAME"

    # Create icon if sips is available and icon.png exists
    if [ -f "$INSTALL_DIR/assets/icon.png" ]; then
        local ICONSET="$RESOURCES/$APP_NAME.iconset"
        mkdir -p "$ICONSET"
        sips -z 16 16     "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_16x16.png"      &>/dev/null
        sips -z 32 32     "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_16x16@2x.png"   &>/dev/null
        sips -z 32 32     "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_32x32.png"      &>/dev/null
        sips -z 64 64     "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_32x32@2x.png"   &>/dev/null
        sips -z 128 128   "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_128x128.png"    &>/dev/null
        sips -z 256 256   "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_128x128@2x.png" &>/dev/null
        sips -z 256 256   "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_256x256.png"    &>/dev/null
        sips -z 512 512   "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_256x256@2x.png" &>/dev/null
        sips -z 512 512   "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_512x512.png"    &>/dev/null
        sips -z 1024 1024 "$INSTALL_DIR/assets/icon.png" --out "$ICONSET/icon_512x512@2x.png" &>/dev/null
        iconutil -c icns "$ICONSET" -o "$RESOURCES/$APP_NAME.icns" 2>/dev/null && {
            echo -e "${GREEN}[OK]${NC} App icon created"
        }
        rm -rf "$ICONSET"
    fi

    # Info.plist
    cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>com.tigrimos.app</string>
    <key>CFBundleVersion</key>
    <string>1.4.0</string>
    <key>CFBundleShortVersionString</key>
    <string>1.4.0</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundleIconFile</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
PLIST

    echo -e "${GREEN}[OK]${NC} App bundle created: $APP_DIR"

    # ── Copy to /Applications ──
    echo ""
    read -rp "Copy $APP_NAME.app to /Applications? [Y/n]: " copy_choice </dev/tty
    if [[ "$copy_choice" != "n" && "$copy_choice" != "N" ]]; then
        if [ -d "/Applications/$APP_NAME.app" ]; then
            rm -rf "/Applications/$APP_NAME.app"
        fi
        cp -R "$APP_DIR" "/Applications/$APP_NAME.app"
        echo -e "${GREEN}[OK]${NC} Installed to /Applications/$APP_NAME.app"
    fi
}

# ── Summary ──
finish() {
    echo ""
    echo -e "${BLUE}========================================${NC}"
    echo -e "${GREEN}  Installation complete!${NC}"
    echo -e "${BLUE}========================================${NC}"
    echo ""
    echo "  Source:  $INSTALL_DIR"
    echo "  Binary:  $INSTALL_DIR/target/release/$BINARY_NAME"
    if [ -d "/Applications/$APP_NAME.app" ]; then
        echo "  App:     /Applications/$APP_NAME.app"
    else
        echo "  App:     $INSTALL_DIR/target/$APP_NAME.app"
    fi
    echo ""
    echo "  To run:  open /Applications/$APP_NAME.app"
    echo "  Or:      $INSTALL_DIR/target/release/$BINARY_NAME"
    echo ""

    read -rp "Launch $APP_NAME now? [Y/n]: " launch </dev/tty
    if [[ "$launch" != "n" && "$launch" != "N" ]]; then
        if [ -d "/Applications/$APP_NAME.app" ]; then
            open "/Applications/$APP_NAME.app"
        else
            open "$INSTALL_DIR/target/$APP_NAME.app"
        fi
    fi
}

# ── Run ──
check_prereqs
select_location
clone_repo
build_app
create_app_bundle
finish
