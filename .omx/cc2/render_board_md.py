#!/usr/bin/env python3
"""Render the Claw Code 2.0 canonical board JSON as a human-readable Markdown board."""
from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

STATUS_DESCRIPTIONS = {
    "context": "Context-only heading or evidence anchor; not an implementation work item.",
    "active": "Current Claw Code 2.0 implementation surface that should remain visible on the board.",
    "open": "Actionable unresolved work that needs implementation or acceptance evidence.",
    "done_verify": "Marked as done upstream but retained for verification against current CC2 behavior.",
    "stale_done": "Historically completed or merged work that may be stale and needs freshness checks before relying on it.",
    "superseded": "Replaced by a newer item; keep as traceability context only.",
    "deferred_with_rationale": "Intentionally deferred; rationale must be present in the board item.",
    "rejected_not_claw": "Excluded because it is not Claw Code product work.",
}

BUCKET_DESCRIPTIONS = {
    "alpha_blocker": "Must be resolved before alpha-quality autonomous coding lanes are dependable.",
    "beta_adoption": "Important for broader dogfood/adoption once alpha blockers are controlled.",
    "ga_ecosystem": "Required for mature plugin/MCP/provider ecosystem behavior.",
    "2.x_intake": "Post-2.0 intake or follow-up candidate retained for sequencing.",
    "post_2_0_research": "Research-oriented item not required for the CC2 board cut.",
    "context": "Non-actionable roadmap context.",
    "rejected_not_claw": "Explicit non-Claw rejection bucket.",
}

LANE_TITLES = {
    "stream_0_governance": "Stream 0 — Governance, intake, and cross-cutting roadmap triage",
    "stream_1_worker_boot_session_control": "Stream 1 — Worker boot and session control",
    "stream_2_event_reporting_contracts": "Stream 2 — Event/reporting contracts",
    "stream_3_branch_test_recovery": "Stream 3 — Branch/test recovery",
    "stream_4_claws_first_execution": "Stream 4 — Claws-first task execution",
    "stream_5_plugin_mcp_lifecycle": "Stream 5 — Plugin/MCP lifecycle",
    "adoption_overlay": "Adoption overlay — user-visible parity and release polish",
    "parity_overlay": "Parity overlay — opencode/codex comparison context",
}

REQUIRED_ITEM_FIELDS = [
    "id",
    "title",
    "source_anchor",
    "source_type",
    "release_bucket",
    "lifecycle_status",
    "dependencies",
    "verification_required",
    "deferral_rationale",
]


def load_board(path: Path) -> dict[str, Any]:
    try:
        with path.open() as f:
            board = json.load(f)
    except FileNotFoundError:
        raise ValueError(f"board not found at {path}") from None
    except IsADirectoryError:
        raise ValueError(f"board path is a directory: {path}") from None
    except json.JSONDecodeError as exc:
        raise ValueError(f"invalid board JSON at {path}: {exc}") from None
    if not isinstance(board, dict):
        raise ValueError("board JSON root must be an object")
    items = board.get("items")
    if not isinstance(items, list):
        raise ValueError("board JSON must contain an items array")
    return board


def validate_board(board: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    coverage = board.get("coverage", {})
    if coverage.get("unmapped_roadmap_heading_lines"):
        errors.append(f"unmapped roadmap heading lines: {coverage['unmapped_roadmap_heading_lines']}")
    if coverage.get("roadmap_headings_mapped") != coverage.get("roadmap_headings_total"):
        errors.append("roadmap heading coverage is incomplete")
    if coverage.get("roadmap_actions_mapped") != coverage.get("roadmap_actions_total"):
        errors.append("roadmap ordered-action coverage is incomplete")

    allowed_status = set(board.get("generation_policy", {}).get("status_values", []))
    allowed_buckets = set(board.get("generation_policy", {}).get("release_buckets", []))
    seen_ids: set[str] = set()
    for index, item in enumerate(board["items"], 1):
        for field in REQUIRED_ITEM_FIELDS:
            if field not in item:
                errors.append(f"item {index} missing required field {field}")
        item_id = item.get("id")
        if item_id in seen_ids:
            errors.append(f"duplicate item id {item_id}")
        seen_ids.add(item_id)
        status = item.get("lifecycle_status")
        bucket = item.get("release_bucket")
        if allowed_status and status not in allowed_status:
            errors.append(f"{item_id} has unknown lifecycle_status {status!r}")
        if allowed_buckets and bucket not in allowed_buckets:
            errors.append(f"{item_id} has unknown release_bucket {bucket!r}")
        if status == "deferred_with_rationale" and not str(item.get("deferral_rationale", "")).strip():
            errors.append(f"{item_id} is deferred without deferral_rationale")
    return errors


def table(headers: list[str], rows: list[list[Any]]) -> list[str]:
    out = ["| " + " | ".join(headers) + " |", "| " + " | ".join("---" for _ in headers) + " |"]
    for row in rows:
        out.append("| " + " | ".join(str(cell) for cell in row) + " |")
    return out


def fmt_list(value: Any) -> str:
    if not value:
        return "none"
    if isinstance(value, list):
        return ", ".join(f"`{v}`" for v in value) if value else "none"
    return f"`{value}`"


def render(board: dict[str, Any]) -> str:
    items: list[dict[str, Any]] = board["items"]
    summary = board.get("summary", {})
    coverage = board.get("coverage", {})
    sources = board.get("sources", {})
    policy = board.get("generation_policy", {})
    by_lane = Counter(item.get("owner_lane", "unassigned") for item in items)
    by_status = Counter(item.get("lifecycle_status", "unknown") for item in items)
    by_bucket = Counter(item.get("release_bucket", "unknown") for item in items)
    by_source = Counter(item.get("source_type", "unknown") for item in items)

    lines: list[str] = []
    lines.append("# Claw Code 2.0 Canonical Board")
    lines.append("")
    lines.append(f"Generated from board schema: `{board.get('generated_at', 'unknown')}`")
    lines.append(f"Schema version: `{board.get('schema_version', 'unknown')}`")
    lines.append("Ultragoal mutation policy: `.omx/ultragoal` is leader-owned and was not modified by this rendering task.")
    lines.append("")

    lines.append("## Evidence Freeze")
    lines.append("")
    roadmap = sources.get("roadmap", {})
    research = sources.get("research", {})
    plan = sources.get("approved_plan", {})
    lines.extend(table(["Source", "Frozen evidence"], [
        ["Roadmap", f"`{roadmap.get('path', 'ROADMAP.md')}` sha256 prefix `{roadmap.get('sha256_prefix', 'unknown')}`; {roadmap.get('heading_count', '?')} headings; {roadmap.get('ordered_action_count', '?')} ordered actions"],
        ["Approved plan", f"`{plan.get('path', '.omx/plans/claw-code-2-0-adaptive-plan.md')}` sha256 prefix `{plan.get('sha256_prefix', 'unknown')}`"],
        ["Research bundle", f"root `{research.get('root', '.omx/research')}`; latest open issues {research.get('claw_open_latest_count', '?')}; issue corpus {research.get('claw_issues_count', '?')}; codex/opencode clone metadata included"],
    ]))
    lines.append("")

    lines.append("## Roadmap Coverage Summary")
    lines.append("")
    heading_total = coverage.get("roadmap_headings_total", 0)
    heading_mapped = coverage.get("roadmap_headings_mapped", 0)
    action_total = coverage.get("roadmap_actions_total", 0)
    action_mapped = coverage.get("roadmap_actions_mapped", 0)
    lines.extend(table(["Coverage gate", "Mapped", "Total", "Status"], [
        ["ROADMAP headings", heading_mapped, heading_total, "PASS" if heading_mapped == heading_total and not coverage.get("unmapped_roadmap_heading_lines") else "FAIL"],
        ["ROADMAP ordered actions", action_mapped, action_total, "PASS" if action_mapped == action_total else "FAIL"],
        ["Duplicate heading lines", len(coverage.get("duplicate_roadmap_heading_lines", [])), 0, "PASS" if not coverage.get("duplicate_roadmap_heading_lines") else "WARN"],
    ]))
    lines.append("")
    lines.append(f"Total canonical board items: **{len(items)}**")
    lines.append("")

    lines.append("## Lifecycle Enum Reference")
    lines.append("")
    status_rows = []
    for status in policy.get("status_values", sorted(by_status)):
        status_rows.append([f"`{status}`", by_status.get(status, 0), STATUS_DESCRIPTIONS.get(status, "Board-defined lifecycle status.")])
    lines.extend(table(["Lifecycle", "Count", "Meaning"], status_rows))
    lines.append("")

    lines.append("## Release Bucket Reference")
    lines.append("")
    bucket_rows = []
    for bucket in policy.get("release_buckets", sorted(by_bucket)):
        bucket_rows.append([f"`{bucket}`", by_bucket.get(bucket, 0), BUCKET_DESCRIPTIONS.get(bucket, "Board-defined release bucket.")])
    lines.extend(table(["Bucket", "Count", "Meaning"], bucket_rows))
    lines.append("")

    lines.append("## Stream Summaries")
    lines.append("")
    lane_rows = []
    for lane, count in sorted(by_lane.items()):
        lane_items = [item for item in items if item.get("owner_lane") == lane]
        lane_status = Counter(item.get("lifecycle_status") for item in lane_items)
        open_like = lane_status.get("active", 0) + lane_status.get("open", 0) + lane_status.get("done_verify", 0)
        lane_rows.append([
            LANE_TITLES.get(lane, lane),
            count,
            open_like,
            ", ".join(f"`{k}` {v}" for k, v in sorted(lane_status.items())),
        ])
    lines.extend(table(["Stream / lane", "Items", "Active+open+verify", "Lifecycle mix"], lane_rows))
    lines.append("")

    lines.append("## Source-Type Mix")
    lines.append("")
    lines.extend(table(["Source type", "Items"], [[f"`{k}`", v] for k, v in sorted(by_source.items())]))
    lines.append("")

    lines.append("## Board Items by Stream")
    lines.append("")
    for lane in sorted(by_lane):
        lane_items = [item for item in items if item.get("owner_lane") == lane]
        lines.append(f"### {LANE_TITLES.get(lane, lane)}")
        lines.append("")
        lines.extend(table(
            ["ID", "Title", "Source", "Bucket", "Lifecycle", "Verification", "Dependencies", "Deferral"],
            [[
                f"`{item.get('id')}`",
                str(item.get("title", "")).replace("|", "\\|"),
                f"`{item.get('source_anchor')}` / `{item.get('source_type')}`",
                f"`{item.get('release_bucket')}`",
                f"`{item.get('lifecycle_status')}`",
                f"`{item.get('verification_required')}`",
                fmt_list(item.get("dependencies")),
                str(item.get("deferral_rationale") or "—").replace("|", "\\|"),
            ] for item in lane_items]
        ))
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("board_json", type=Path)
    parser.add_argument("board_md", type=Path)
    parser.add_argument("--check", action="store_true", help="fail if board_md is not up to date")
    args = parser.parse_args()

    try:
        board = load_board(args.board_json)
    except ValueError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    errors = validate_board(board)
    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1
    rendered = render(board)
    if args.check:
        try:
            existing = args.board_md.read_text() if args.board_md.exists() else ""
        except IsADirectoryError:
            print(f"ERROR: board markdown path is a directory: {args.board_md}", file=sys.stderr)
            return 1
        if existing != rendered:
            print(f"ERROR: {args.board_md} is not up to date", file=sys.stderr)
            return 1
        print(f"PASS: {args.board_md} is up to date and roadmap coverage is complete")
        return 0
    args.board_md.parent.mkdir(parents=True, exist_ok=True)
    try:
        args.board_md.write_text(rendered)
    except IsADirectoryError:
        print(f"ERROR: board markdown path is a directory: {args.board_md}", file=sys.stderr)
        return 1
    print(f"wrote {args.board_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
