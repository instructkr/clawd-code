#!/usr/bin/env python3
"""Canonical CC2 board command wrapper.

This script intentionally delegates to the richer G001 board generator,
validator, and Markdown renderer so all entrypoints enforce the same schema.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


def run(cmd: list[str], cwd: Path) -> int:
    return subprocess.run(cmd, cwd=str(cwd)).returncode


def run_quiet_until_failure(cmd: list[str], cwd: Path) -> int:
    result = subprocess.run(cmd, cwd=str(cwd), text=True, capture_output=True)
    if result.returncode:
        if result.stdout:
            print(result.stdout, end="")
        if result.stderr:
            print(result.stderr, end="", file=sys.stderr)
    return result.returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["generate", "validate"])
    parser.add_argument("--repo-root", type=Path, default=Path.cwd(), help="repository root containing ROADMAP.md")
    parser.add_argument("--context-root", type=Path, default=None, help="accepted for compatibility; source .omx is auto-detected by the generator")
    parser.add_argument("--board-json", default=".omx/cc2/board.json")
    parser.add_argument("--board-md", default=".omx/cc2/board.md")
    args = parser.parse_args(argv)

    repo_root = args.repo_root.resolve()
    script_root = Path(__file__).resolve().parent
    tool_root = script_root.parent
    board_json = repo_root / args.board_json
    board_md = repo_root / args.board_md
    generator = script_root / "generate_cc2_board.py"
    validator = script_root / "validate_cc2_board.py"
    renderer = tool_root / ".omx" / "cc2" / "render_board_md.py"

    if args.command == "generate":
        rc = run([sys.executable, str(generator), "--repo-root", str(repo_root), "--out-dir", str(board_json.parent)], repo_root)
        if rc:
            return rc
        return run([sys.executable, str(renderer), str(board_json), str(board_md)], repo_root)

    checks = [
        [sys.executable, str(validator), "--repo-root", str(repo_root), "--board", str(board_json)],
        [sys.executable, str(renderer), str(board_json), str(board_md), "--check"],
    ]
    for cmd in checks:
        rc = run_quiet_until_failure(cmd, repo_root)
        if rc:
            return rc
    print(f"CC2 board validation PASS: {board_json} and {board_md} are canonical and in sync")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
