#!/bin/bash
set -e

# Build the release binary
cargo build --release

# Link to ~/.local/bin
mkdir -p "$HOME/.local/bin"
ln -sf "$(pwd)/target/release/claw" "$HOME/.local/bin/claw"

echo "✓ Claw installed to ~/.local/bin/claw"
