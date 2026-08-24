#!/usr/bin/env python3
"""Validate the generated Claw Code 2.0 board coverage and schema."""
from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

REQUIRED = {
    "id",
    "title",
    "source_anchor",
    "source_type",
    "release_bucket",
    "status",
    "dependencies",
    "verification_required",
    "deferral_rationale",
}
STATUSES = {
    "context",
    "active",
    "open",
    "done_verify",
    "stale_done",
    "superseded",
    "deferred_with_rationale",
    "rejected_not_claw",
}

def roadmap_heading_lines(path: Path) -> list[int]:
    lines = []
    for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if re.match(r"^#{1,6}\s+", line):
            lines.append(line_no)
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--board", type=Path, default=None)
    args = parser.parse_args()
    repo_root = args.repo_root.resolve()
    board_path = args.board or (repo_root / ".omx" / "cc2" / "board.json")
    try:
        board = json.loads(board_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        print(f"error: board not found at {board_path}")
        return 1
    except IsADirectoryError:
        print(f"error: board path is a directory: {board_path}")
        return 1
    except json.JSONDecodeError as exc:
        print(f"error: invalid board JSON at {board_path}: {exc}")
        return 1
    errors: list[str] = []
    ids = set()
    for index, item in enumerate(board.get("items", []), 1):
        missing = REQUIRED - set(item)
        if missing:
            errors.append(f"item {index} missing required fields: {sorted(missing)}")
        if item.get("id") in ids:
            errors.append(f"duplicate id: {item.get('id')}")
        ids.add(item.get("id"))
        if item.get("status") not in STATUSES:
            errors.append(f"{item.get('id')} invalid status {item.get('status')}")
        if not isinstance(item.get("dependencies"), list):
            errors.append(f"{item.get('id')} dependencies must be list")
    expected = roadmap_heading_lines(repo_root / "ROADMAP.md")
    mapped = [item.get("source_line") for item in board.get("items", []) if item.get("source_type") == "roadmap_heading"]
    unmapped = sorted(set(expected) - set(mapped))
    duplicates = sorted(line for line in set(mapped) if mapped.count(line) != 1)
    if unmapped:
        errors.append(f"unmapped ROADMAP headings: {unmapped}")
    if duplicates:
        errors.append(f"duplicate ROADMAP heading mappings: {duplicates}")
    coverage = board.get("coverage", {})
    if coverage.get("roadmap_headings_total") != len(expected):
        errors.append("coverage roadmap_headings_total does not match ROADMAP.md")
    if coverage.get("roadmap_headings_mapped") != len(mapped):
        errors.append("coverage roadmap_headings_mapped does not match board items")
    if errors:
        print("FAIL cc2 board validation")
        for error in errors:
            print(f"- {error}")
        return 1
    print("PASS cc2 board validation")
    print(f"- board: {board_path}")
    print(f"- items: {len(board.get('items', []))}")
    print(f"- ROADMAP headings mapped: {len(mapped)}/{len(expected)}")
    print(f"- ROADMAP actions mapped: {coverage.get('roadmap_actions_mapped')}/{coverage.get('roadmap_actions_total')}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
