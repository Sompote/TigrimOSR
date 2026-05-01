#!/bin/bash
# TigrimOS Installer for Linux
# Clones, builds, and creates a .desktop entry

APP_NAME="TigrimOS"
REPO_URL="https://github.com/Sompote/TigrimOSR.git"
BINARY_NAME="tigrimos"

# ── Colors ──
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

# Helper: read from terminal even when piped
prompt() {
    local var_name="$1" prompt_text="$2" default="$3"
    if [ -t 0 ]; then
        read -rp "$prompt_text" "$var_name"
    elif [ -e /dev/tty ]; then
        read -rp "$prompt_text" "$var_name" < /dev/tty
    else
        eval "$var_name='$default'"
    fi
}

die() {
    echo -e "${RED}[ERROR] $1${NC}"
    exit 1
}

echo ""
echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}  TigrimOS Installer for Linux${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""

# ── Check prerequisites ──
echo "Checking prerequisites..."
command -v git &>/dev/null  || die "git not found. Install: sudo apt install git / sudo dnf install git / sudo pacman -S git"
command -v rustc &>/dev/null || die "rustc not found. Install: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
command -v cargo &>/dev/null || die "cargo not found. Install: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
echo -e "${GREEN}[OK]${NC} Prerequisites found (git, rustc, cargo)"

# Check for common build dependencies (needed for egui/eframe)
dev_missing=()
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
    prompt cont "Continue anyway? [Y/n]: " "y"
    [[ "$cont" == "n" || "$cont" == "N" ]] && exit 1
fi

# ── Select install location ──
echo ""
echo -e "${YELLOW}Where would you like to install TigrimOS?${NC}"
echo ""
echo "  1) Home directory       (~/$APP_NAME)"
echo "  2) Opt directory        (/opt/$APP_NAME)"
echo "  3) Custom location"
echo ""

prompt choice "Select [1-3] (default: 1): " ""

case "$choice" in
    2) INSTALL_DIR="/opt/$APP_NAME" ;;
    3)
        prompt custom_path "Enter full path: " ""
        [ -z "$custom_path" ] && die "No path provided."
        INSTALL_DIR="$custom_path"
        ;;
    *) INSTALL_DIR="$HOME/$APP_NAME" ;;
esac

echo -e "${BLUE}Install location:${NC} $INSTALL_DIR"

# ── Clone or update repo ──
echo ""
if [ -d "$INSTALL_DIR/.git" ]; then
    echo -e "${YELLOW}Existing installation found. Updating...${NC}"
    cd "$INSTALL_DIR" || die "Cannot cd to $INSTALL_DIR"
    git pull --ff-only || echo -e "${YELLOW}Pull failed, continuing with existing code...${NC}"
else
    if [ -d "$INSTALL_DIR" ]; then
        echo -e "${YELLOW}Removing existing non-git directory at $INSTALL_DIR...${NC}"
        rm -rf "$INSTALL_DIR"
    fi
    echo -e "${BLUE}Cloning TigrimOS...${NC}"
    mkdir -p "$(dirname "$INSTALL_DIR")"
    git clone "$REPO_URL" "$INSTALL_DIR" || die "git clone failed"
fi

cd "$INSTALL_DIR" || die "Cannot cd to $INSTALL_DIR"
[ -f "$INSTALL_DIR/Cargo.toml" ] || die "Cargo.toml not found in $INSTALL_DIR — clone may have failed"
echo -e "${GREEN}[OK]${NC} Source ready at $INSTALL_DIR"

# ── Build ──
echo ""
echo -e "${BLUE}Building TigrimOS (release mode)...${NC}"
echo "This may take a few minutes on first build."
echo ""

cargo build --release || die "Build failed"
[ -f "$INSTALL_DIR/target/release/$BINARY_NAME" ] || die "Binary not found after build"

echo ""
echo -e "${GREEN}[OK]${NC} Build complete"

# ── Create dist folder ──
DIST_DIR="$INSTALL_DIR/dist"
echo ""
echo -e "${BLUE}Creating distribution...${NC}"

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

cp "$INSTALL_DIR/target/release/$BINARY_NAME" "$DIST_DIR/$BINARY_NAME"
chmod +x "$DIST_DIR/$BINARY_NAME"

if [ -f "$INSTALL_DIR/assets/icon.png" ]; then
    cp "$INSTALL_DIR/assets/icon.png" "$DIST_DIR/icon.png"
fi

echo -e "${GREEN}[OK]${NC} Distribution created: $DIST_DIR"

# ── Create .desktop entry ──
echo ""
prompt create_entry "Create application menu entry (.desktop)? [Y/n]: " "y"
if [[ "$create_entry" != "n" && "$create_entry" != "N" ]]; then
    DESKTOP_DIR="$HOME/.local/share/applications"
    mkdir -p "$DESKTOP_DIR"

    ICON_PATH=""
    if [ -f "$DIST_DIR/icon.png" ]; then
        ICON_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"
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

    if command -v update-desktop-database &>/dev/null; then
        update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true
    fi

    echo -e "${GREEN}[OK]${NC} Application menu entry created"

    # Desktop shortcut
    prompt create_shortcut "Create desktop shortcut? [Y/n]: " "y"
    if [[ "$create_shortcut" != "n" && "$create_shortcut" != "N" ]]; then
        DESKTOP_FILE="$HOME/Desktop/tigrimos.desktop"
        cp "$DESKTOP_DIR/tigrimos.desktop" "$DESKTOP_FILE"
        chmod +x "$DESKTOP_FILE"
        if command -v gio &>/dev/null; then
            gio set "$DESKTOP_FILE" metadata::trusted true 2>/dev/null || true
        fi
        echo -e "${GREEN}[OK]${NC} Desktop shortcut created"
    fi
fi

# ── Add to PATH ──
echo ""
prompt add_path "Add tigrimos to PATH (symlink to ~/.local/bin)? [Y/n]: " "y"
if [[ "$add_path" != "n" && "$add_path" != "N" ]]; then
    mkdir -p "$HOME/.local/bin"
    ln -sf "$DIST_DIR/$BINARY_NAME" "$HOME/.local/bin/$BINARY_NAME"
    echo -e "${GREEN}[OK]${NC} Symlinked to ~/.local/bin/$BINARY_NAME"

    if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
        echo -e "${YELLOW}Note: Add ~/.local/bin to your PATH if not already:${NC}"
        echo '  export PATH="$HOME/.local/bin:$PATH"'
    fi
fi

# ── Summary ──
echo ""
echo -e "${BLUE}========================================${NC}"
echo -e "${GREEN}  Installation complete!${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""
echo "  Source:  $INSTALL_DIR"
echo "  Binary:  $DIST_DIR/$BINARY_NAME"
echo ""
echo "  To run:  $DIST_DIR/$BINARY_NAME"
[ -f "$HOME/.local/bin/$BINARY_NAME" ] && echo "  Or:      $BINARY_NAME  (via PATH)"
echo ""

prompt launch_choice "Launch $APP_NAME now? [Y/n]: " "y"
if [[ "$launch_choice" != "n" && "$launch_choice" != "N" ]]; then
    nohup "$DIST_DIR/$BINARY_NAME" &>/dev/null &
    echo -e "${GREEN}Launched!${NC}"
fi
