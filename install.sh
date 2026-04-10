#!/usr/bin/env bash
set -euo pipefail

# ── merlint installer ──
# curl -fsSL https://raw.githubusercontent.com/Link817290/Merlint/main/install.sh | bash

REPO="https://github.com/Link817290/Merlint.git"
INSTALL_DIR="${MERLINT_INSTALL_DIR:-/tmp/merlint-install}"

# Colors
R='\033[0;31m' G='\033[0;32m' Y='\033[0;33m' B='\033[0;34m'
M='\033[0;35m' C='\033[0;36m' W='\033[1;37m' D='\033[0;90m' N='\033[0m'

banner() {
    echo ""
    echo -e "${C}    ╔═══════════════════════════════════════════════════╗${N}"
    echo -e "${C}    ║${N}                                                   ${C}║${N}"
    echo -e "${C}    ║${N}         ${M}/\\${N}                                        ${C}║${N}"
    echo -e "${C}    ║${N}        ${M}/  \\${N}     ${G}                 _ _       _${N}    ${C}║${N}"
    echo -e "${C}    ║${N}       ${M}/____\\${N}    ${G} _ __ ___   ___ | (_)_ __ | |_${N}  ${C}║${N}"
    echo -e "${C}    ║${N}       ${Y}(O  O)${N}    ${G}| '_ \` _ \\ / _ \\| | | '_ \\| __|${N} ${C}║${N}"
    echo -e "${C}    ║${N}        ${Y}<>${N}      ${G}| | | | | |  __/| | | | | | |_ ${N} ${C}║${N}"
    echo -e "${C}    ║${N}       ${W}/|  |\\${N}    ${G}|_| |_| |_|\\___||_|_|_| |_|\\__|${N} ${C}║${N}"
    echo -e "${C}    ║${N}      ${Y}*---+${M}~${N}                                        ${C}║${N}"
    echo -e "${C}    ║${N}                                                   ${C}║${N}"
    echo -e "${C}    ║${N}  ${D}  Agent Token Optimizer                  v0.1.3${N}  ${C}║${N}"
    echo -e "${C}    ╚═══════════════════════════════════════════════════╝${N}"
    echo ""
}

info()  { echo -e "  ${B}[*]${N} $1"; }
ok()    { echo -e "  ${G}[+]${N} $1"; }
warn()  { echo -e "  ${Y}[!]${N} $1"; }
fail()  { echo -e "  ${R}[x]${N} $1"; exit 1; }

banner

# ── Check OS ──
OS="$(uname -s)"
ARCH="$(uname -m)"
info "Detected: ${W}${OS}${N} / ${W}${ARCH}${N}"

case "$OS" in
    Linux|Darwin) ;;
    *) fail "Unsupported OS: $OS (Linux and macOS only)" ;;
esac

# ── Check / Install Rust ──
if command -v cargo &>/dev/null; then
    RUST_VER="$(rustc --version 2>/dev/null || echo 'unknown')"
    ok "Rust already installed: ${D}${RUST_VER}${N}"
else
    info "Rust not found. Installing via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --quiet
    export PATH="$HOME/.cargo/bin:$PATH"
    ok "Rust installed: $(rustc --version)"
fi

# ── Check git ──
if ! command -v git &>/dev/null; then
    fail "git is required but not installed. Please install git first."
fi

# ── Clone & Install ──
info "Installing merlint..."

if [ -d "$INSTALL_DIR" ]; then
    rm -rf "$INSTALL_DIR"
fi

git clone --depth 1 --quiet "$REPO" "$INSTALL_DIR" 2>/dev/null || {
    if [ -f "Cargo.toml" ] && grep -q 'name = "merlint"' Cargo.toml 2>/dev/null; then
        INSTALL_DIR="."
        warn "Could not clone repo, installing from current directory"
    else
        fail "Could not clone repository. Check your network connection."
    fi
}

cd "$INSTALL_DIR"
cargo install --path . --quiet 2>&1 | while read -r line; do
    echo -e "  ${D}  ${line}${N}"
done

# ── Verify ──
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v merlint &>/dev/null; then
    fail "Installation failed. merlint not found in PATH."
fi

ok "merlint installed: ${D}$(which merlint)${N}"

# ── Create launcher script ──
info "Setting up auto-proxy launcher..."

MERLINT_DIR="$HOME/.merlint"
mkdir -p "$MERLINT_DIR"

cat > "$MERLINT_DIR/proxy.sh" << 'LAUNCHER'
#!/usr/bin/env bash
# merlint auto-proxy launcher
# Starts merlint proxy in background and sets ANTHROPIC_BASE_URL

MERLINT_PORT="${MERLINT_PORT:-8019}"
MERLINT_LOG="$HOME/.merlint/proxy.log"
MERLINT_PID="$HOME/.merlint/proxy.pid"

merlint-start() {
    # Check if already running
    if [ -f "$MERLINT_PID" ] && kill -0 "$(cat "$MERLINT_PID")" 2>/dev/null; then
        return 0
    fi

    # Start proxy in background
    nohup merlint proxy \
        --target https://api.anthropic.com \
        --optimize \
        --port "$MERLINT_PORT" \
        --daemon \
        > "$MERLINT_LOG" 2>&1 &

    echo $! > "$MERLINT_PID"
    export ANTHROPIC_BASE_URL="http://127.0.0.1:${MERLINT_PORT}"
}

merlint-stop() {
    if [ -f "$MERLINT_PID" ]; then
        kill "$(cat "$MERLINT_PID")" 2>/dev/null || true
        rm -f "$MERLINT_PID"
    fi
    unset ANTHROPIC_BASE_URL
}

merlint-status() {
    if [ -f "$MERLINT_PID" ] && kill -0 "$(cat "$MERLINT_PID")" 2>/dev/null; then
        echo "merlint proxy running (PID $(cat "$MERLINT_PID"), port ${MERLINT_PORT})"
    else
        echo "merlint proxy not running"
    fi
}

# Auto-start on shell init
merlint-start
LAUNCHER

chmod +x "$MERLINT_DIR/proxy.sh"
ok "Launcher created: ${D}~/.merlint/proxy.sh${N}"

# ── Add to shell profile ──
SHELL_LINE='[ -f "$HOME/.merlint/proxy.sh" ] && source "$HOME/.merlint/proxy.sh"'

add_to_profile() {
    local file="$1"
    if [ -f "$file" ]; then
        if ! grep -qF '.merlint/proxy.sh' "$file" 2>/dev/null; then
            echo "" >> "$file"
            echo "# merlint auto-proxy (token optimizer for Claude Code)" >> "$file"
            echo "$SHELL_LINE" >> "$file"
            ok "Added to ${D}${file}${N}"
            return 0
        else
            ok "Already in ${D}${file}${N}"
            return 0
        fi
    fi
    return 1
}

CONFIGURED=false

# Detect shell and add to the right profile
case "${SHELL:-}" in
    */zsh)
        add_to_profile "$HOME/.zshrc" && CONFIGURED=true
        ;;
    */bash)
        add_to_profile "$HOME/.bashrc" && CONFIGURED=true
        ;;
    *)
        # Try both
        add_to_profile "$HOME/.zshrc" && CONFIGURED=true
        add_to_profile "$HOME/.bashrc" && CONFIGURED=true
        ;;
esac

# Also try .profile as fallback
if [ "$CONFIGURED" = false ]; then
    add_to_profile "$HOME/.profile" && CONFIGURED=true
fi

if [ "$CONFIGURED" = false ]; then
    warn "Could not find shell profile. Add this line manually:"
    echo ""
    echo "    $SHELL_LINE"
    echo ""
fi

# ── Start proxy now ──
info "Starting merlint proxy..."
source "$MERLINT_DIR/proxy.sh"

sleep 0.5
if [ -f "$MERLINT_DIR/proxy.pid" ] && kill -0 "$(cat "$MERLINT_DIR/proxy.pid")" 2>/dev/null; then
    ok "Proxy running on port ${MERLINT_PORT:-8019}"
else
    warn "Proxy may not have started. Check ~/.merlint/proxy.log"
fi

# ── Done ──
echo ""
echo -e "  ${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${N}"
echo -e "  ${G}  Installation complete! merlint is active.${N}"
echo -e "  ${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${N}"
echo ""
echo -e "  ${W}What happens now:${N}"
echo -e "    Every time you open a terminal, merlint proxy auto-starts."
echo -e "    Claude Code requests go through merlint and get optimized."
echo -e "    ${D}You don't need to do anything else.${N}"
echo ""
echo -e "  ${W}Commands:${N}"
echo -e "    ${C}merlint-status${N}    ${D}# check if proxy is running${N}"
echo -e "    ${C}merlint-stop${N}      ${D}# stop the proxy${N}"
echo -e "    ${C}merlint-start${N}     ${D}# restart the proxy${N}"
echo -e "    ${C}merlint latest${N}    ${D}# analyze your latest session${N}"
echo -e "    ${C}merlint scan${N}      ${D}# browse all sessions${N}"
echo ""

# Cleanup
if [ "$INSTALL_DIR" != "." ] && [ -d "$INSTALL_DIR" ]; then
    rm -rf "$INSTALL_DIR"
fi
