#!/usr/bin/env bash
# dogfood-build.sh — Build claw from current checkout and verify provenance.
#
# Injects GIT_SHA at build time so version JSON is non-null.
# Suppresses Cargo compile noise on stderr.
# Prints the verified binary path on success. Use as:
#
#   CLAW=$(bash scripts/dogfood-build.sh)
#
# Then dogfood with config isolation (avoids real user config bleeding in):
#
#   CLAW_CONFIG_HOME=$(mktemp -d) $CLAW plugins list --output-format json
#
set -euo pipefail

usage() {
    sed -n '2,12p' "$0" | sed 's/^# //; s/^#//'
}

if [[ $# -gt 0 ]]; then
    case "$1" in
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$REPO_ROOT/rust"
BINARY="$RUST_DIR/target/debug/claw"
EXPECTED_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"

echo "▶ Building claw from $REPO_ROOT" >&2
echo "  Commit: $(git -C "$REPO_ROOT" log --oneline -1)" >&2

# Inject GIT_SHA so version JSON returns a non-null sha.
# Redirect cargo stderr to /dev/null to suppress compile noise;
# on build failure cargo exits non-zero and set -e aborts.
if ! GIT_SHA="$EXPECTED_SHA" cargo build \
        --manifest-path "$RUST_DIR/Cargo.toml" \
        -p rusty-claude-cli -q 2>/dev/null; then
    # Re-run with visible output so the user sees the error
    echo "✗ Build failed — rerunning with output:" >&2
    GIT_SHA="$EXPECTED_SHA" cargo build \
        --manifest-path "$RUST_DIR/Cargo.toml" \
        -p rusty-claude-cli 2>&1 | sed 's/^/  /' >&2
    exit 1
fi

if [[ ! -x "$BINARY" ]]; then
    echo "✗ Binary not found at $BINARY" >&2
    exit 1
fi

BINARY_SHA=$("$BINARY" version --output-format json 2>/dev/null \
    | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('git_sha') or 'null')" 2>/dev/null \
    || echo "null")

if [[ "$BINARY_SHA" == "null" || -z "$BINARY_SHA" ]]; then
    echo "✗ Provenance check failed: binary reports git_sha: null" >&2
    exit 1
fi

if [[ "$BINARY_SHA" != "$EXPECTED_SHA" ]]; then
    echo "✗ Provenance mismatch: binary=$BINARY_SHA, HEAD=$EXPECTED_SHA" >&2
    exit 1
fi

echo "✓ Binary verified: $BINARY_SHA == HEAD" >&2
echo "" >&2
echo "  export CLAW=$BINARY" >&2
echo "" >&2
echo "  Dogfood with isolated config (no real user config on stderr):" >&2
echo "    CLAW_ISOLATED=\$(mktemp -d)" >&2
echo "    trap 'rm -rf \"\$CLAW_ISOLATED\"' EXIT" >&2
echo "    CLAW_CONFIG_HOME=\$CLAW_ISOLATED \$CLAW plugins list --output-format json" >&2
echo "" >&2
echo "  cargo run overhead: ~1s/invocation vs 7ms for pre-built binary." >&2
echo "  Prefer pre-built binary (\$CLAW) for dogfood loops." >&2
echo "$BINARY"
