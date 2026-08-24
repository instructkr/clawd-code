#!/usr/bin/env bash
# Claw Code installer
#
# Detects the host OS, verifies the Rust toolchain (rustc + cargo),
# builds the `claw` binary from the `rust/` workspace, and runs a
# post-install verification step. Supports Linux, macOS, and WSL.
#
# Usage:
#   ./install.sh                # debug build (fast, default)
#   ./install.sh --release      # optimized release build
#   ./install.sh --no-verify    # skip post-install verification
#   ./install.sh --help         # print usage
#
# Environment overrides:
#   CLAW_BUILD_PROFILE=debug|release   same as --release toggle
#   CLAW_SKIP_VERIFY=1                 same as --no-verify

set -euo pipefail

# ---------------------------------------------------------------------------
# Pretty printing
# ---------------------------------------------------------------------------

if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]; then
    COLOR_RESET="$(tput sgr0)"
    COLOR_BOLD="$(tput bold)"
    COLOR_DIM="$(tput dim)"
    COLOR_RED="$(tput setaf 1)"
    COLOR_GREEN="$(tput setaf 2)"
    COLOR_YELLOW="$(tput setaf 3)"
    COLOR_BLUE="$(tput setaf 4)"
    COLOR_CYAN="$(tput setaf 6)"
else
    COLOR_RESET=""
    COLOR_BOLD=""
    COLOR_DIM=""
    COLOR_RED=""
    COLOR_GREEN=""
    COLOR_YELLOW=""
    COLOR_BLUE=""
    COLOR_CYAN=""
fi

CURRENT_STEP=0
TOTAL_STEPS=6

step() {
    CURRENT_STEP=$((CURRENT_STEP + 1))
    printf '\n%s[%d/%d]%s %s%s%s\n' \
        "${COLOR_BLUE}" "${CURRENT_STEP}" "${TOTAL_STEPS}" "${COLOR_RESET}" \
        "${COLOR_BOLD}" "$1" "${COLOR_RESET}"
}

info()  { printf '%s  ->%s %s\n' "${COLOR_CYAN}" "${COLOR_RESET}" "$1"; }
ok()    { printf '%s  ok%s %s\n' "${COLOR_GREEN}" "${COLOR_RESET}" "$1"; }
warn()  { printf '%s  warn%s %s\n' "${COLOR_YELLOW}" "${COLOR_RESET}" "$1"; }
error() { printf '%s  error%s %s\n' "${COLOR_RED}" "${COLOR_RESET}" "$1" 1>&2; }

print_banner() {
    printf '%s' "${COLOR_BOLD}"
    cat <<'EOF'
   ____  _                   ____          _
  / ___|| |  __ _ __      __ / ___|___   __| | ___
 | |    | | / _` |\ \ /\ / /| |   / _ \ / _` |/ _ \
 | |___ | || (_| | \ V  V / | |__| (_) | (_| |  __/
  \____||_| \__,_|  \_/\_/   \____\___/ \__,_|\___|
EOF
    printf '%s\n' "${COLOR_RESET}"
    printf '%sClaw Code installer%s\n' "${COLOR_DIM}" "${COLOR_RESET}"
}

print_usage() {
    cat <<'EOF'
Usage: ./install.sh [options]

Options:
  --release       Build the optimized release profile (slower, smaller binary).
  --debug         Build the debug profile (default, faster compile).
  --no-verify     Skip the post-install verification step.
  -h, --help      Show this help text and exit.

Environment overrides:
  CLAW_BUILD_PROFILE   debug | release
  CLAW_SKIP_VERIFY     set to 1 to skip verification
EOF
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

BUILD_PROFILE="${CLAW_BUILD_PROFILE:-debug}"
SKIP_VERIFY="${CLAW_SKIP_VERIFY:-0}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --release)
            BUILD_PROFILE="release"
            ;;
        --debug)
            BUILD_PROFILE="debug"
            ;;
        --no-verify)
            SKIP_VERIFY="1"
            ;;
        -h|--help)
            print_usage
            exit 0
            ;;
        *)
            error "unknown argument: $1"
            print_usage
            exit 2
            ;;
    esac
    shift
done

case "${BUILD_PROFILE}" in
    debug|release) ;;
    *)
        error "invalid build profile: ${BUILD_PROFILE} (expected debug or release)"
        exit 2
        ;;
esac

# ---------------------------------------------------------------------------
# Troubleshooting hints
# ---------------------------------------------------------------------------

print_troubleshooting() {
    cat <<EOF

${COLOR_BOLD}Troubleshooting${COLOR_RESET}
${COLOR_DIM}---------------${COLOR_RESET}

  ${COLOR_BOLD}1. Rust toolchain missing${COLOR_RESET}
     Install Rust via rustup:
       curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
     Then reload your shell or run:
       source "\$HOME/.cargo/env"

  ${COLOR_BOLD}2. Linux: missing system packages${COLOR_RESET}
     The build needs git, pkg-config, and OpenSSL headers.
     Debian/Ubuntu:
       sudo apt-get update && sudo apt-get install -y \\
         git pkg-config libssl-dev ca-certificates build-essential
     Fedora/RHEL:
       sudo dnf install -y git pkgconf-pkg-config openssl-devel gcc
     Arch:
       sudo pacman -S --needed git pkgconf openssl base-devel

  ${COLOR_BOLD}3. macOS: missing Xcode CLT${COLOR_RESET}
     Install the command line tools:
       xcode-select --install

  ${COLOR_BOLD}4. Windows users${COLOR_RESET}
     Run this script from inside a WSL distro (Ubuntu/Debian recommended).
     Native Windows builds are not supported by this installer.

  ${COLOR_BOLD}5. Build fails partway through${COLOR_RESET}
     Try a clean build:
       cd rust && cargo clean && cargo build --workspace
     If the failure mentions ring/openssl, double check step 2.

  ${COLOR_BOLD}6. 'claw' not found after install${COLOR_RESET}
     The binary lives at:
       rust/target/${BUILD_PROFILE}/claw
     Add it to your PATH or invoke it with the full path.

EOF
}

trap 'rc=$?; if [ "$rc" -ne 0 ]; then error "installation failed (exit ${rc})"; print_troubleshooting; fi' EXIT

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

require_cmd() {
    command -v "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------------------
# Step 1: detect OS / arch / WSL
# ---------------------------------------------------------------------------

print_banner
step "Detecting host environment"

UNAME_S="$(uname -s 2>/dev/null || echo unknown)"
UNAME_M="$(uname -m 2>/dev/null || echo unknown)"
OS_FAMILY="unknown"
IS_WSL="0"

case "${UNAME_S}" in
    Linux*)
        OS_FAMILY="linux"
        if grep -qiE 'microsoft|wsl' /proc/version 2>/dev/null; then
            IS_WSL="1"
        fi
        ;;
    Darwin*)
        OS_FAMILY="macos"
        ;;
    MINGW*|MSYS*|CYGWIN*)
        OS_FAMILY="windows-shell"
        ;;
esac

info "uname:        ${UNAME_S} ${UNAME_M}"
info "os family:    ${OS_FAMILY}"
if [ "${IS_WSL}" = "1" ]; then
    info "wsl:          yes"
fi

case "${OS_FAMILY}" in
    linux|macos)
        ok "supported platform detected"
        ;;
    windows-shell)
        error "Detected a native Windows shell (MSYS/Cygwin/MinGW)."
        error "Please re-run this script from inside a WSL distribution."
        exit 1
        ;;
    *)
        error "Unsupported or unknown OS: ${UNAME_S}"
        error "Supported: Linux, macOS, and Windows via WSL."
        exit 1
        ;;
esac

# ---------------------------------------------------------------------------
# Step 2: locate the Rust workspace
# ---------------------------------------------------------------------------

step "Locating the Rust workspace"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUST_DIR="${SCRIPT_DIR}/rust"

if [ ! -d "${RUST_DIR}" ]; then
    error "Could not find rust/ workspace next to install.sh"
    error "Expected: ${RUST_DIR}"
    exit 1
fi

if [ ! -f "${RUST_DIR}/Cargo.toml" ]; then
    error "Missing ${RUST_DIR}/Cargo.toml — repository layout looks unexpected."
    exit 1
fi

ok "workspace at ${RUST_DIR}"

# ---------------------------------------------------------------------------
# Step 3: prerequisite checks
# ---------------------------------------------------------------------------

step "Checking prerequisites"

MISSING_PREREQS=0

if require_cmd rustc; then
    RUSTC_VERSION="$(rustc --version 2>/dev/null || echo 'unknown')"
    ok "rustc found: ${RUSTC_VERSION}"
else
    error "rustc not found in PATH"
    MISSING_PREREQS=1
fi

if require_cmd cargo; then
    CARGO_VERSION="$(cargo --version 2>/dev/null || echo 'unknown')"
    ok "cargo found: ${CARGO_VERSION}"
else
    error "cargo not found in PATH"
    MISSING_PREREQS=1
fi

if require_cmd git; then
    ok "git found:  $(git --version 2>/dev/null || echo 'unknown')"
else
    warn "git not found — some workflows (login, session export) may degrade"
fi

if [ "${OS_FAMILY}" = "linux" ]; then
    if require_cmd pkg-config; then
        ok "pkg-config found"
    else
        warn "pkg-config not found — may be required for OpenSSL-linked crates"
    fi
fi

if [ "${OS_FAMILY}" = "macos" ]; then
    if ! require_cmd cc && ! xcode-select -p >/dev/null 2>&1; then
        warn "Xcode command line tools not detected — run: xcode-select --install"
    fi
fi

if [ "${MISSING_PREREQS}" -ne 0 ]; then
    error "Missing required tools. See troubleshooting below."
    exit 1
fi

# ---------------------------------------------------------------------------
# Step 4: build the workspace
# ---------------------------------------------------------------------------

step "Building the claw workspace (${BUILD_PROFILE})"

CARGO_FLAGS=("build" "--workspace")
if [ "${BUILD_PROFILE}" = "release" ]; then
    CARGO_FLAGS+=("--release")
fi

info "running: cargo ${CARGO_FLAGS[*]}"
info "this may take a few minutes on the first build"

(
    cd "${RUST_DIR}"
    CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}" cargo "${CARGO_FLAGS[@]}"
)

CLAW_BIN="${RUST_DIR}/target/${BUILD_PROFILE}/claw"

if [ ! -x "${CLAW_BIN}" ]; then
    error "Expected binary not found at ${CLAW_BIN}"
    error "The build reported success but the binary is missing — check cargo output above."
    exit 1
fi

ok "built ${CLAW_BIN}"

# ---------------------------------------------------------------------------
# Step 5: post-install verification
# ---------------------------------------------------------------------------

step "Verifying the installed binary"

if [ "${SKIP_VERIFY}" = "1" ]; then
    warn "verification skipped (--no-verify or CLAW_SKIP_VERIFY=1)"
else
    info "running: claw --version"
    if VERSION_OUT="$("${CLAW_BIN}" --version 2>&1)"; then
        ok "claw --version -> ${VERSION_OUT}"
    else
        error "claw --version failed:"
        printf '%s\n' "${VERSION_OUT}" 1>&2
        exit 1
    fi

    info "running: claw --help (smoke test)"
    if "${CLAW_BIN}" --help >/dev/null 2>&1; then
        ok "claw --help responded"
    else
        error "claw --help failed"
        exit 1
    fi
fi

# ---------------------------------------------------------------------------
# Step 6: next steps
# ---------------------------------------------------------------------------

step "Next steps"

cat <<EOF
${COLOR_GREEN}Claw Code is built and ready.${COLOR_RESET}

  Binary:  ${COLOR_BOLD}${CLAW_BIN}${COLOR_RESET}
  Profile: ${BUILD_PROFILE}

Try it out:

  ${COLOR_DIM}# interactive REPL${COLOR_RESET}
  ${CLAW_BIN}

  ${COLOR_DIM}# one-shot prompt${COLOR_RESET}
  ${CLAW_BIN} prompt "summarize this repository"

  ${COLOR_DIM}# health check (run /doctor inside the REPL)${COLOR_RESET}
  ${CLAW_BIN}
  /doctor

Authentication:

  export ANTHROPIC_API_KEY="sk-ant-..."
  ${COLOR_DIM}# or use OAuth:${COLOR_RESET}
  ${CLAW_BIN} login

For deeper docs, see USAGE.md and rust/README.md.
EOF

# clear the failure trap on clean exit
trap - EXIT
