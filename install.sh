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
    echo -e "${C}    ║${N}  ${D}  Agent Token Optimizer                  v0.1.8${N}  ${C}║${N}"
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

# ── Auto-configure shell hook ──
info "Configuring shell hook for auto-proxy..."
merlint setup-shell 2>/dev/null && ok "Shell hook installed (auto-proxy on new terminals)" \
    || warn "Could not install shell hook. Run ${C}merlint setup-shell${N} manually."

# ── Done ──
echo ""
echo -e "  ${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${N}"
echo -e "  ${G}  Installation complete!${N}"
echo -e "  ${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${N}"
echo ""
echo -e "  ${W}Start the proxy:${N}"
echo ""
echo -e "    ${C}merlint up${N}"
echo ""
echo -e "  ${D}Then set ANTHROPIC_BASE_URL as shown and use Claude Code normally.${N}"
echo ""
echo -e "  ${W}Other commands:${N}"
echo -e "    ${C}merlint up${N}            ${D}# start proxy (background)${N}"
echo -e "    ${C}merlint up --fg${N}       ${D}# start proxy (foreground with logs)${N}"
echo -e "    ${C}merlint down${N}          ${D}# stop proxy${N}"
echo -e "    ${C}merlint dashboard${N}     ${D}# live monitoring TUI${N}"
echo -e "    ${C}merlint latest${N}        ${D}# analyze your latest session${N}"
echo -e "    ${C}merlint scan${N}          ${D}# browse all sessions${N}"
echo ""

# Cleanup
if [ "$INSTALL_DIR" != "." ] && [ -d "$INSTALL_DIR" ]; then
    rm -rf "$INSTALL_DIR"
fi
