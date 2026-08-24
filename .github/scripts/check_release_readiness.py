#!/usr/bin/env python3
"""Validate release-readiness docs that are easy to regress.

The check is intentionally dependency-free so it can run on developer machines,
Windows CI, and minimal release jobs. It validates:

* required repository policy files exist;
* local Markdown links and image targets resolve;
* local heading anchors referenced from Markdown resolve; and
* command examples do not present the deprecated `cargo install claw-code`
  package as an executable install path.
"""

from __future__ import annotations

from pathlib import Path
from urllib.parse import unquote, urlparse
import re
import sys

ROOT = Path(__file__).resolve().parents[2]

REQUIRED_POLICY_FILES = [
    "LICENSE",
    "CONTRIBUTING.md",
    "SECURITY.md",
    "SUPPORT.md",
    "CODE_OF_CONDUCT.md",
]

MARKDOWN_ROOTS = [
    ROOT / "README.md",
    ROOT / "USAGE.md",
    ROOT / "PARITY.md",
    ROOT / "PHILOSOPHY.md",
    ROOT / "ROADMAP.md",
    ROOT / "CONTRIBUTING.md",
    ROOT / "SECURITY.md",
    ROOT / "SUPPORT.md",
    ROOT / "CODE_OF_CONDUCT.md",
    ROOT / "docs",
    ROOT / "rust" / "README.md",
    ROOT / "rust" / "USAGE.md",
    ROOT / "rust" / "MOCK_PARITY_HARNESS.md",
]

LINK_PATTERN = re.compile(r"(?<!!)\[[^\]\n]+\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
HTML_LINK_PATTERN = re.compile(r"""<(?:a|img)\b[^>]*(?:href|src)=["']([^"']+)["']""", re.I)
FENCE_PATTERN = re.compile(r"```(?P<lang>[^\n`]*)\n(?P<body>.*?)```", re.S)


def iter_markdown_files() -> list[Path]:
    files: set[Path] = set()
    for entry in MARKDOWN_ROOTS:
        if entry.is_file():
            files.add(entry)
        elif entry.is_dir():
            files.update(entry.rglob("*.md"))
    return sorted(files)


def github_anchor(heading: str) -> str:
    anchor = heading.strip().lower()
    anchor = re.sub(r"<[^>]+>", "", anchor)
    anchor = re.sub(r"`([^`]*)`", r"\1", anchor)
    anchor = re.sub(r"[^a-z0-9 _-]", "", anchor)
    anchor = anchor.replace(" ", "-")
    anchor = re.sub(r"-+", "-", anchor)
    return anchor.strip("-")


def anchors_for(path: Path) -> set[str]:
    anchors: set[str] = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        match = re.match(r"^(#{1,6})\s+(.+?)\s*#*\s*$", line)
        if match:
            anchors.add(github_anchor(match.group(2)))
    return anchors


def is_external(target: str) -> bool:
    parsed = urlparse(target)
    return parsed.scheme in {"http", "https", "mailto"}


def validate_policies(errors: list[str]) -> None:
    for relative in REQUIRED_POLICY_FILES:
        path = ROOT / relative
        if not path.is_file():
            errors.append(f"missing required policy file: {relative}")


def validate_markdown_links(errors: list[str]) -> None:
    anchor_cache: dict[Path, set[str]] = {}
    for path in iter_markdown_files():
        text = path.read_text(encoding="utf-8")
        candidates = [m.group(1) for m in LINK_PATTERN.finditer(text)]
        candidates.extend(m.group(1) for m in HTML_LINK_PATTERN.finditer(text))
        for target in candidates:
            if (
                not target
                or is_external(target)
                or target.startswith(("mailto:", "tel:", "data:"))
            ):
                continue
            link_path, _, raw_anchor = target.partition("#")
            if not link_path:
                destination = path
            else:
                destination = (path.parent / unquote(link_path)).resolve()
            try:
                destination.relative_to(ROOT)
            except ValueError:
                errors.append(
                    f"{path.relative_to(ROOT)}: link escapes repo root: {target}"
                )
                continue
            if not destination.exists():
                errors.append(
                    f"{path.relative_to(ROOT)}: missing local link target: {target}"
                )
                continue
            if raw_anchor and destination.suffix.lower() == ".md":
                anchor = unquote(raw_anchor).lower()
                anchor_cache.setdefault(destination, anchors_for(destination))
                if anchor not in anchor_cache[destination]:
                    errors.append(
                        f"{path.relative_to(ROOT)}: missing anchor `{raw_anchor}` in "
                        f"{destination.relative_to(ROOT)}"
                    )


def validate_command_examples(errors: list[str]) -> None:
    for path in iter_markdown_files():
        text = path.read_text(encoding="utf-8")
        for match in FENCE_PATTERN.finditer(text):
            lang = match.group("lang").strip().lower()
            if lang not in {"bash", "sh", "shell", "zsh", "powershell", "ps1"}:
                continue
            body = match.group("body")
            for offset, line in enumerate(body.splitlines(), start=1):
                stripped = line.strip()
                if not stripped or stripped.startswith(("#", ">")):
                    continue
                if re.search(r"\bcargo\s+install\s+claw-code\b", stripped):
                    line_no = text.count("\n", 0, match.start()) + offset + 1
                    errors.append(
                        f"{path.relative_to(ROOT)}:{line_no}: deprecated "
                        "`cargo install claw-code` appears in an executable "
                        "command block; use build-from-source docs instead"
                    )


def main() -> int:
    errors: list[str] = []
    validate_policies(errors)
    validate_markdown_links(errors)
    validate_command_examples(errors)
    if errors:
        print("release-readiness check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("release-readiness check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
