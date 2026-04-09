#!/usr/bin/env bash
set -euo pipefail

# ── agentbench installer ──
# curl -fsSL https://raw.githubusercontent.com/user/agentbench/main/install.sh | bash

REPO="https://github.com/user/agentbench.git"
INSTALL_DIR="${AGENTBENCH_INSTALL_DIR:-/tmp/agentbench-install}"

# Colors
R='\033[0;31m' G='\033[0;32m' Y='\033[0;33m' B='\033[0;34m'
M='\033[0;35m' C='\033[0;36m' W='\033[1;37m' D='\033[0;90m' N='\033[0m'

banner() {
    echo ""
    echo -e "${C}    ╔══════════════════════════════════════════════════════╗${N}"
    echo -e "${C}    ║${N}${W}        _                 _   _                     ${N}${C}║${N}"
    echo -e "${C}    ║${N}${G}   __ _ ${Y}| |__   ___ ${R}_ __ | |_${M}| |__   ___ ${B}_ __   ___ ${N}${C}║${N}"
    echo -e "${C}    ║${N}${G}  / _\` |${Y}| '_ \\ / _ \\\\${R}| '_ \\| __${M}| '_ \\ / _ \\\\${B}| '_ \\ / __|${N}${C}║${N}"
    echo -e "${C}    ║${N}${G} | (_| |${Y}| |_) |  __/${R}| | | | |_${M}| |_) |  __/${B}| | | | (__ ${N}${C}║${N}"
    echo -e "${C}    ║${N}${G}  \\__,_|${Y}|_.__/ \\___|${R}|_| |_|\\__${M}|_.__/ \\___|${B}|_| |_|\\___|${N}${C}║${N}"
    echo -e "${C}    ║${N}                                                      ${C}║${N}"
    echo -e "${C}    ║${N}${D}     Agent Execution Efficiency Analyzer  v0.1.0     ${N}${C}║${N}"
    echo -e "${C}    ╚══════════════════════════════════════════════════════╝${N}"
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
info "Installing agentbench..."

if [ -d "$INSTALL_DIR" ]; then
    rm -rf "$INSTALL_DIR"
fi

git clone --depth 1 --quiet "$REPO" "$INSTALL_DIR" 2>/dev/null || {
    # If clone fails (e.g. repo doesn't exist yet), check if we're in the repo dir
    if [ -f "Cargo.toml" ] && grep -q 'name = "agentbench"' Cargo.toml 2>/dev/null; then
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
if command -v agentbench &>/dev/null; then
    echo ""
    echo -e "  ${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${N}"
    echo -e "  ${G}  Installation complete!${N}"
    echo -e "  ${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${N}"
    echo ""
    echo -e "  ${W}Quick start:${N}"
    echo -e "    ${C}agentbench scan${N}           ${D}# find agent sessions${N}"
    echo -e "    ${C}agentbench latest${N}         ${D}# analyze most recent session${N}"
    echo -e "    ${C}agentbench proxy -d${N}       ${D}# start daemon proxy${N}"
    echo -e "    ${C}agentbench query${N}          ${D}# JSON output for agents${N}"
    echo ""
    echo -e "  ${D}Binary: $(which agentbench)${N}"
    echo ""
else
    fail "Installation failed. agentbench not found in PATH."
fi

# Cleanup
if [ "$INSTALL_DIR" != "." ] && [ -d "$INSTALL_DIR" ]; then
    rm -rf "$INSTALL_DIR"
fi
