#!/usr/bin/env bash
# roadmap-next-id.sh — print the next available ROADMAP item id.
# Usage: scripts/roadmap-next-id.sh [path/to/ROADMAP.md]
#
# Designed to be used before appending a new entry so that concurrent
# dogfood claws do not accidentally reuse the same id:
#
#   NEXT=$(scripts/roadmap-next-id.sh)
#   cat >> ROADMAP.md << EOF
#   ${NEXT}. **...description...**
#   EOF
#
# The script first validates helper-era ids with roadmap-check-ids.sh, then
# reads the highest numeric id prefix from ROADMAP.md and prints highest+1. It
# does not lock the file; callers working in parallel should git-pull
# immediately before appending, run scripts/roadmap-check-ids.sh before push,
# and resolve any append collision at git-push time.
set -euo pipefail

ROADMAP="ROADMAP.md"
ROADMAP_PATH_SEEN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h)
      sed -n '2,15p' "$0" | sed 's/^# //; s/^#//'
      exit 0
      ;;
    --*)
      echo "error: unknown option: $1" >&2
      exit 2
      ;;
    *)
      if [[ "$ROADMAP_PATH_SEEN" -ne 0 ]]; then
        echo "error: unexpected extra ROADMAP path: $1" >&2
        exit 2
      fi
      ROADMAP="$1"
      ROADMAP_PATH_SEEN=1
      shift
      ;;
  esac
done

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
CHECKER="$SCRIPT_DIR/roadmap-check-ids.sh"

if [[ ! -f "$ROADMAP" ]]; then
  echo "error: ROADMAP not found at $ROADMAP" >&2
  exit 1
fi

if [[ ! -f "$CHECKER" || ! -r "$CHECKER" ]]; then
  echo "error: required ROADMAP id checker not found or not readable at $CHECKER" >&2
  echo "error: refusing to print a next id without duplicate-id validation" >&2
  exit 1
fi

if ! checker_output="$(bash "$CHECKER" "$ROADMAP" 2>&1)"; then
  printf '%s\n' "$checker_output" >&2
  exit 1
fi

# Find the highest leading integer from lines that start with a number + '.'.
highest=$(awk '
  /^[0-9]+\./ {
    id = $0
    sub(/\..*/, "", id)
    id += 0
    if (id > highest) {
      highest = id
    }
  }
  END { print highest + 0 }
' "$ROADMAP")

if [[ "$highest" -eq 0 ]]; then
  echo 1
else
  echo $(( highest + 1 ))
fi
