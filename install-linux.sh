#!/bin/bash
# TigrimOS Installer for Linux
# Clones, builds, and creates a .desktop entry

APP_NAME="TigrimOS"
REPO_URL="https://github.com/Sompote/TigrimOSR.git"
BINARY_NAME="tigrimos"

# Ensure we're in a readable directory (curl | bash may inherit a restricted cwd)
cd "$HOME" 2>/dev/null || cd /tmp 2>/dev/null || true

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

# Auto-install essential build dependencies (OpenSSL, pkg-config)
if ! command -v pkg-config &>/dev/null || ! pkg-config --exists openssl 2>/dev/null; then
    echo -e "${YELLOW}Installing build dependencies (pkg-config, libssl-dev)...${NC}"
    if command -v apt &>/dev/null; then
        sudo apt update && sudo apt install -y pkg-config libssl-dev build-essential
    elif command -v dnf &>/dev/null; then
        sudo dnf install -y pkg-config openssl-devel gcc make
    elif command -v pacman &>/dev/null; then
        sudo pacman -S --noconfirm pkg-config openssl base-devel
    fi
    echo -e "${GREEN}[OK]${NC} Build dependencies installed"
fi

# Check for GUI dependencies (only needed for desktop mode)
dev_missing=()
for lib in libxcb libxkbcommon libgtk-3; do
    if ! pkg-config --exists "$lib" 2>/dev/null; then
        dev_missing+=("$lib-dev")
    fi
done

if [ ${#dev_missing[@]} -gt 0 ]; then
    echo -e "${YELLOW}[WARN] GUI libraries missing: ${dev_missing[*]}${NC}"
    echo "  These are only needed for desktop mode (not headless)."
    echo ""
    echo "  To install (Debian/Ubuntu):"
    echo "    sudo apt install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libgtk-3-dev"
    echo ""
    prompt cont "Continue anyway? (headless mode works without these) [Y/n]: " "y"
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

# ── Install mode: Desktop or Headless (remote server) ──
echo ""
echo -e "${YELLOW}How do you want to run TigrimOS?${NC}"
echo ""
echo "  1) Desktop mode     (GUI app — requires display)"
echo "  2) Headless mode    (remote server — no GUI, access via web browser)"
echo ""
prompt run_mode "Select [1-2] (default: 1): " ""

HEADLESS=false
[[ "$run_mode" == "2" ]] && HEADLESS=true

if $HEADLESS; then
    echo ""
    echo -e "${BLUE}========================================${NC}"
    echo -e "${BLUE}  Headless (Remote Server) Setup${NC}"
    echo -e "${BLUE}========================================${NC}"
    echo ""

    # Port
    prompt server_port "Server port [3001]: " "3001"
    [ -z "$server_port" ] && server_port="3001"

    # Access token
    while true; do
        prompt access_token "Access token (min 8 chars): " ""
        if [ ${#access_token} -ge 8 ]; then
            break
        fi
        echo -e "${RED}Token too short — must be at least 8 characters.${NC}"
    done

    # ── Create systemd service ──
    echo ""
    prompt create_service "Create systemd service (auto-start on boot)? [Y/n]: " "y"
    if [[ "$create_service" != "n" && "$create_service" != "N" ]]; then
        SERVICE_FILE="/etc/systemd/system/tigrimos.service"
        echo -e "${BLUE}Creating systemd service...${NC}"

        sudo tee "$SERVICE_FILE" > /dev/null <<SVCEOF
[Unit]
Description=TigrimOS Headless Server
After=network.target

[Service]
Type=simple
User=$USER
WorkingDirectory=$INSTALL_DIR
ExecStart=$DIST_DIR/$BINARY_NAME --headless
Environment=ACCESS_TOKEN=$access_token
Environment=PORT=$server_port
Environment=SANDBOX_DIR=$INSTALL_DIR/sandbox
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
SVCEOF

        sudo systemctl daemon-reload
        sudo systemctl enable tigrimos
        echo -e "${GREEN}[OK]${NC} systemd service created and enabled"
        echo "  Start:   sudo systemctl start tigrimos"
        echo "  Stop:    sudo systemctl stop tigrimos"
        echo "  Logs:    sudo journalctl -u tigrimos -f"
    fi

    # ── Setup nginx reverse proxy ──
    echo ""
    prompt setup_nginx "Setup nginx reverse proxy (access via port 80/443)? [Y/n]: " "y"
    if [[ "$setup_nginx" != "n" && "$setup_nginx" != "N" ]]; then
        # Install nginx if not present
        if ! command -v nginx &>/dev/null; then
            echo -e "${BLUE}Installing nginx...${NC}"
            if command -v apt &>/dev/null; then
                sudo apt update && sudo apt install -y nginx
            elif command -v dnf &>/dev/null; then
                sudo dnf install -y nginx
            elif command -v pacman &>/dev/null; then
                sudo pacman -S --noconfirm nginx
            else
                echo -e "${RED}Could not detect package manager. Install nginx manually.${NC}"
            fi
        fi

        if command -v nginx &>/dev/null; then
            prompt server_domain "Server domain or IP (e.g. example.com or _ for any): " "_"
            [ -z "$server_domain" ] && server_domain="_"

            NGINX_CONF="/etc/nginx/sites-available/tigrimos"
            NGINX_ENABLED="/etc/nginx/sites-enabled/tigrimos"

            sudo tee "$NGINX_CONF" > /dev/null <<NGXEOF
server {
    listen 80;
    server_name $server_domain;

    client_max_body_size 50M;

    location / {
        proxy_pass http://127.0.0.1:$server_port;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;

        # WebSocket support
        proxy_http_version 1.1;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection "upgrade";

        # Long timeout for AI responses
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
    }
}
NGXEOF

            # Enable the site
            if [ -d "/etc/nginx/sites-enabled" ]; then
                sudo ln -sf "$NGINX_CONF" "$NGINX_ENABLED"
            fi

            # Test and reload
            if sudo nginx -t 2>/dev/null; then
                sudo systemctl reload nginx 2>/dev/null || sudo systemctl restart nginx
                echo -e "${GREEN}[OK]${NC} nginx configured and reloaded"
            else
                echo -e "${RED}nginx config test failed. Check: sudo nginx -t${NC}"
            fi

            # Offer SSL setup
            echo ""
            if [[ "$server_domain" != "_" ]] && command -v certbot &>/dev/null; then
                prompt setup_ssl "Setup HTTPS with Let's Encrypt? [y/N]: " "n"
                if [[ "$setup_ssl" == "y" || "$setup_ssl" == "Y" ]]; then
                    sudo certbot --nginx -d "$server_domain"
                    echo -e "${GREEN}[OK]${NC} HTTPS configured"
                fi
            elif [[ "$server_domain" != "_" ]]; then
                echo -e "${YELLOW}Tip: Install certbot for HTTPS:${NC}"
                echo "  sudo apt install certbot python3-certbot-nginx"
                echo "  sudo certbot --nginx -d $server_domain"
            fi
        fi
    fi

    # ── Open firewall ──
    echo ""
    if command -v ufw &>/dev/null; then
        prompt open_fw "Open firewall port $server_port (ufw)? [Y/n]: " "y"
        if [[ "$open_fw" != "n" && "$open_fw" != "N" ]]; then
            sudo ufw allow "$server_port/tcp"
            sudo ufw allow 80/tcp
            sudo ufw allow 443/tcp
            echo -e "${GREEN}[OK]${NC} Firewall ports opened"
        fi
    fi

    # ── Summary ──
    echo ""
    echo -e "${BLUE}========================================${NC}"
    echo -e "${GREEN}  Headless Installation Complete!${NC}"
    echo -e "${BLUE}========================================${NC}"
    echo ""
    echo "  Source:   $INSTALL_DIR"
    echo "  Binary:   $DIST_DIR/$BINARY_NAME"
    echo "  Port:     $server_port"
    echo "  Token:    $access_token"
    echo ""
    echo -e "${YELLOW}  Access:${NC}"
    echo "    Web UI:     http://<server-ip>:$server_port/web/"
    if [[ "$setup_nginx" != "n" && "$setup_nginx" != "N" ]] && command -v nginx &>/dev/null; then
        echo "    Via nginx:  http://<server-ip>/web/"
    fi
    echo ""
    echo -e "${YELLOW}  Commands:${NC}"
    echo "    Start:  sudo systemctl start tigrimos"
    echo "    Stop:   sudo systemctl stop tigrimos"
    echo "    Logs:   sudo journalctl -u tigrimos -f"
    echo "    Manual: ACCESS_TOKEN=$access_token PORT=$server_port $DIST_DIR/$BINARY_NAME --headless"
    echo ""

    prompt launch_now "Start TigrimOS server now? [Y/n]: " "y"
    if [[ "$launch_now" != "n" && "$launch_now" != "N" ]]; then
        if [ -f "/etc/systemd/system/tigrimos.service" ]; then
            sudo systemctl start tigrimos
            echo -e "${GREEN}Server started via systemd!${NC}"
            echo "  Check: sudo systemctl status tigrimos"
        else
            echo "Starting in background..."
            ACCESS_TOKEN="$access_token" PORT="$server_port" nohup "$DIST_DIR/$BINARY_NAME" --headless &>/dev/null &
            echo -e "${GREEN}Server started! PID: $!${NC}"
        fi
    fi
else
    # ── Desktop mode summary ──
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
fi
