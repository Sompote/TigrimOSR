#!/bin/bash
# TigrimOS Installer for Linux
# Clones, builds, and creates a .desktop entry

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
echo -e "${BLUE}  TigrimOS Installer for Linux${NC}"
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
            echo "  Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        fi
        if [[ " ${missing[*]} " =~ " git " ]]; then
            echo "  Install git:  sudo apt install git  (Debian/Ubuntu)"
            echo "                sudo dnf install git  (Fedora)"
            echo "                sudo pacman -S git    (Arch)"
        fi
        exit 1
    fi

    # Check for common build dependencies (needed for egui/eframe)
    local dev_missing=()
    for lib in libxcb libxkbcommon libgtk-3; do
        if ! pkg-config --exists "$lib" 2>/dev/null; then
            dev_missing+=("$lib-dev")
        fi
    done

    if [ ${#dev_missing[@]} -gt 0 ]; then
        echo -e "${YELLOW}[WARN] Possibly missing dev libraries: ${dev_missing[*]}${NC}"
        echo ""
        echo "  Debian/Ubuntu: sudo apt install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libgtk-3-dev"
        echo "  Fedora:        sudo dnf install libxcb-devel libxkbcommon-devel gtk3-devel"
        echo "  Arch:          sudo pacman -S libxcb libxkbcommon gtk3"
        echo ""
        read -rp "Continue anyway? [Y/n]: " cont
        if [[ "$cont" == "n" || "$cont" == "N" ]]; then
            exit 1
        fi
    fi

    echo -e "${GREEN}[OK]${NC} Prerequisites found (git, rustc, cargo)"
}

# ── Select install location ──
select_location() {
    echo ""
    echo -e "${YELLOW}Where would you like to install TigrimOS?${NC}"
    echo ""
    echo "  1) Home directory       (~/$APP_NAME)"
    echo "  2) Opt directory        (/opt/$APP_NAME)"
    echo "  3) Custom location"
    echo ""
    read -rp "Select [1-3] (default: 1): " choice

    case "$choice" in
        2) INSTALL_DIR="/opt/$APP_NAME" ;;
        3)
            read -rp "Enter full path: " custom_path
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
        mkdir -p "$(dirname "$INSTALL_DIR")"
        git clone "$REPO_URL" "$INSTALL_DIR"
        cd "$INSTALL_DIR"
    fi
    echo -e "${GREEN}[OK]${NC} Source ready"
}

# ── Build ──
build_app() {
    echo ""
    echo -e "${BLUE}Building TigrimOS (release mode)...${NC}"
    echo "This may take a few minutes on first build."
    echo ""
    cargo build --release 2>&1 | tail -5
    echo ""
    echo -e "${GREEN}[OK]${NC} Build complete"
}

# ── Create dist folder ──
create_distribution() {
    local DIST_DIR="$INSTALL_DIR/dist"

    echo ""
    echo -e "${BLUE}Creating distribution...${NC}"

    rm -rf "$DIST_DIR"
    mkdir -p "$DIST_DIR"

    # Copy binary
    cp "$INSTALL_DIR/target/release/$BINARY_NAME" "$DIST_DIR/$BINARY_NAME"
    chmod +x "$DIST_DIR/$BINARY_NAME"

    # Copy icon
    if [ -f "$INSTALL_DIR/assets/icon.png" ]; then
        cp "$INSTALL_DIR/assets/icon.png" "$DIST_DIR/icon.png"
    fi

    echo -e "${GREEN}[OK]${NC} Distribution created: $DIST_DIR"
}

# ── Create .desktop entry ──
create_desktop_entry() {
    local DIST_DIR="$INSTALL_DIR/dist"

    echo ""
    read -rp "Create application menu entry (.desktop)? [Y/n]: " create_entry
    if [[ "$create_entry" == "n" || "$create_entry" == "N" ]]; then
        return
    fi

    local DESKTOP_DIR="$HOME/.local/share/applications"
    mkdir -p "$DESKTOP_DIR"

    local ICON_PATH=""
    if [ -f "$DIST_DIR/icon.png" ]; then
        # Install icon to standard location
        local ICON_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"
        mkdir -p "$ICON_DIR"
        cp "$DIST_DIR/icon.png" "$ICON_DIR/tigrimos.png"
        ICON_PATH="tigrimos"
    fi

    cat > "$DESKTOP_DIR/tigrimos.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=$APP_NAME
Comment=AI Agent Platform
Exec=$DIST_DIR/$BINARY_NAME
Icon=${ICON_PATH:-application-default-icon}
Terminal=false
Categories=Development;Utility;
StartupWMClass=tigrimos
DESKTOP

    chmod +x "$DESKTOP_DIR/tigrimos.desktop"

    # Update desktop database if available
    if command -v update-desktop-database &>/dev/null; then
        update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true
    fi

    echo -e "${GREEN}[OK]${NC} Application menu entry created"

    # Desktop shortcut
    read -rp "Create desktop shortcut? [Y/n]: " create_shortcut
    if [[ "$create_shortcut" != "n" && "$create_shortcut" != "N" ]]; then
        local DESKTOP_FILE="$HOME/Desktop/tigrimos.desktop"
        cp "$DESKTOP_DIR/tigrimos.desktop" "$DESKTOP_FILE"
        chmod +x "$DESKTOP_FILE"
        # Mark as trusted on GNOME
        if command -v gio &>/dev/null; then
            gio set "$DESKTOP_FILE" metadata::trusted true 2>/dev/null || true
        fi
        echo -e "${GREEN}[OK]${NC} Desktop shortcut created"
    fi
}

# ── Add to PATH ──
add_to_path() {
    local DIST_DIR="$INSTALL_DIR/dist"

    echo ""
    read -rp "Add tigrimos to PATH (symlink to ~/.local/bin)? [Y/n]: " add_path
    if [[ "$add_path" == "n" || "$add_path" == "N" ]]; then
        return
    fi

    mkdir -p "$HOME/.local/bin"
    ln -sf "$DIST_DIR/$BINARY_NAME" "$HOME/.local/bin/$BINARY_NAME"
    echo -e "${GREEN}[OK]${NC} Symlinked to ~/.local/bin/$BINARY_NAME"

    if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
        echo -e "${YELLOW}Note: Add ~/.local/bin to your PATH if not already:${NC}"
        echo '  export PATH="$HOME/.local/bin:$PATH"'
    fi
}

# ── Summary ──
finish() {
    local DIST_DIR="$INSTALL_DIR/dist"

    echo ""
    echo -e "${BLUE}========================================${NC}"
    echo -e "${GREEN}  Installation complete!${NC}"
    echo -e "${BLUE}========================================${NC}"
    echo ""
    echo "  Source:  $INSTALL_DIR"
    echo "  Binary:  $DIST_DIR/$BINARY_NAME"
    echo ""
    echo "  To run:  $DIST_DIR/$BINARY_NAME"
    if [ -f "$HOME/.local/bin/$BINARY_NAME" ]; then
        echo "  Or:      $BINARY_NAME  (via PATH)"
    fi
    echo ""

    read -rp "Launch $APP_NAME now? [Y/n]: " launch
    if [[ "$launch" != "n" && "$launch" != "N" ]]; then
        nohup "$DIST_DIR/$BINARY_NAME" &>/dev/null &
        echo -e "${GREEN}Launched!${NC}"
    fi
}

# ── Run ──
check_prereqs
select_location
clone_repo
build_app
create_distribution
create_desktop_entry
add_to_path
finish
