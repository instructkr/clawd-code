#!/usr/bin/env python3
"""Generate the canonical Claw Code 2.0 execution board from frozen roadmap evidence."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

REQUIRED_ITEM_FIELDS = [
    "id",
    "title",
    "source_anchor",
    "source_type",
    "release_bucket",
    "status",
    "dependencies",
    "verification_required",
    "deferral_rationale",
]
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
RELEASE_BUCKETS = {
    "alpha_blocker",
    "beta_adoption",
    "ga_ecosystem",
    "post_2_0_research",
    "rejected_not_claw",
    "context",
    "2.x_intake",
}

STRUCTURAL_HEADINGS = {
    "Clawable Coding Harness Roadmap",
    "Goal",
    'Definition of "clawable"',
    "Current Pain Points",
    "Product Principles",
    "Roadmap",
    "Immediate Backlog (from current real pain)",
    "Deployment Architecture Gap (filed from dogfood 2026-04-08)",
    "Startup Friction Gap: No Default trusted_roots in Settings (filed 2026-04-08)",
    "Observability Transport Decision (filed 2026-04-08)",
    "Provider Routing: Model-Name Prefix Must Win Over Env-Var Presence (fixed 2026-04-08, `0530c50`)",
}

CATEGORY_KEYWORDS = [
    ("security", ["security", "sandbox", "permission", "trust", "approval-token", "denied"]),
    ("windows_install", ["windows", "install", "path", "release", "binary", "container"]),
    ("provider", ["provider", "model", "openai", "anthropic", "ollama", "llama", "vllm", "credential"]),
    ("sessions", ["session", "resume", "compact", "context-window", "thread"]),
    ("docs_license", ["docs", "readme", "usage", "license", "help", "onboarding"]),
    ("ide_acp", ["zed", "acp", "editor", "daemon"]),
    ("plugin_mcp", ["plugin", "mcp", "marketplace", "server"]),
    ("event_report", ["event", "report", "schema", "projection", "redaction", "clawhip", "lane"]),
    ("branch_recovery", ["branch", "stale", "recovery", "green", "flake"]),
    ("boot", ["boot", "worker", "startup", "ready", "prompt"]),
    ("task_policy", ["task", "policy", "claw-native", "dashboard", "lane board"]),
    ("ux_tui", ["tui", "statusline", "keymap", "clickable", "copy", "paste"]),
    ("anti_slop", ["spam", "slop", "issue hygiene", "bot"]),
]

@dataclass(frozen=True)
class RoadmapRecord:
    line: int
    level: int
    title: str
    path: str
    source_type: str
    ordinal: int | None = None


def sha256_prefix(path: Path, length: int = 16) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()[:length]


def slugify(text: str, limit: int = 54) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
    return slug[:limit].strip("-") or "item"


def find_source_omx(repo_root: Path) -> Path:
    candidates = []
    env = None
    try:
        import os
        env = os.environ.get("CC2_SOURCE_OMX")
    except Exception:
        env = None
    if env:
        candidates.append(Path(env).expanduser())
    candidates.append(repo_root / ".omx")
    candidates.extend(parent / ".omx" for parent in repo_root.parents)
    for candidate in candidates:
        if (candidate / "plans" / "claw-code-2-0-adaptive-plan.md").exists() and (candidate / "research").exists():
            return candidate
    raise FileNotFoundError("could not locate source .omx with plans/claw-code-2-0-adaptive-plan.md and research/")


def parse_roadmap(path: Path) -> tuple[list[RoadmapRecord], list[RoadmapRecord]]:
    headings: list[RoadmapRecord] = []
    actions: list[RoadmapRecord] = []
    stack: list[tuple[str, int, int]] = []
    for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        heading = re.match(r"^(#{1,6})\s+(.*?)(?:\s+#+)?\s*$", line)
        if heading:
            level = len(heading.group(1))
            title = heading.group(2).strip()
            stack = [entry for entry in stack if entry[1] < level] + [(title, level, line_no)]
            headings.append(RoadmapRecord(line_no, level, title, " > ".join(entry[0] for entry in stack), "roadmap_heading"))
            continue
        ordered = re.match(r"^(\s*)(\d+)\.\s+(.+?)\s*$", line)
        if ordered and len(ordered.group(1)) <= 4:
            title = ordered.group(3).strip()
            if len(title) > 10:
                actions.append(
                    RoadmapRecord(
                        line_no,
                        len(stack[-1][0]) if stack else 0,
                        title,
                        " > ".join(entry[0] for entry in stack),
                        "roadmap_action",
                        int(ordered.group(2)),
                    )
                )
    return headings, actions


def category_for(text: str) -> str:
    lower = text.lower()
    for category, needles in CATEGORY_KEYWORDS:
        if any(needle in lower for needle in needles):
            return category
    return "governance"


def stream_for(record: RoadmapRecord) -> str:
    title = record.title.lower()
    path = record.path.lower()
    combined = f"{path} {title}"
    if "phase 1" in combined or category_for(combined) == "boot":
        return "stream_1_worker_boot_session_control"
    if "phase 2" in combined or category_for(combined) == "event_report":
        return "stream_2_event_reporting_contracts"
    if "phase 3" in combined or category_for(combined) == "branch_recovery":
        return "stream_3_branch_test_recovery"
    if "phase 4" in combined or category_for(combined) == "task_policy":
        return "stream_4_claws_first_execution"
    if "phase 5" in combined or category_for(combined) == "plugin_mcp":
        return "stream_5_plugin_mcp_lifecycle"
    if any(k in combined for k in ["windows", "install", "provider", "docs", "license", "session hygiene", "compact"]):
        return "adoption_overlay"
    if any(k in combined for k in ["zed", "acp", "desktop", "marketplace", "package"]):
        return "parity_overlay"
    return "stream_0_governance"


def release_bucket_for(record: RoadmapRecord, status: str) -> str:
    combined = f"{record.path} {record.title}".lower()
    category = category_for(combined)
    if status == "context":
        return "context"
    if status == "rejected_not_claw":
        return "rejected_not_claw"
    if any(k in combined for k in ["phase 1", "phase 2", "phase 3", "phase 4", "p0", "p1", "security", "sandbox", "trust", "worker", "event", "branch freshness"]):
        return "alpha_blocker"
    if category in {"windows_install", "provider", "sessions", "docs_license", "anti_slop"}:
        return "beta_adoption"
    if category in {"plugin_mcp", "ide_acp", "ux_tui"}:
        return "ga_ecosystem"
    if any(k in combined for k in ["desktop", "share", "cloud", "research", "post-2.0", "future"]):
        return "post_2_0_research"
    if "pinpoint" in combined:
        return "alpha_blocker"
    return "beta_adoption"


def status_for(record: RoadmapRecord) -> str:
    title = record.title
    combined = f"{record.path} {title}".lower()
    if record.source_type == "roadmap_heading" and (record.level <= 2 or title in STRUCTURAL_HEADINGS):
        # Phase headings are active work containers; other h1/h2 prose headings are context unless fixed/deferred wording says otherwise.
        if title.startswith("Phase "):
            return "active"
        if "pinpoint" not in title.lower() and not any(word in combined for word in ["gap", "routing"]):
            return "context"
    if any(word in combined for word in ["rejected_not_claw", "not claw", "outside claw"]):
        return "rejected_not_claw"
    if "superseded" in combined:
        return "superseded"
    if "deferred" in combined or "post-2.0" in combined or "post_2_0" in combined:
        return "deferred_with_rationale"
    if any(word in combined for word in ["done", "implemented", "fixed", "verified", "re-verified", "landed", "green"]):
        if any(word in combined for word in ["stale", "old filing", "original filing below", "no longer reproduces"]):
            return "stale_done"
        return "done_verify"
    if title.lower().startswith(("evidence for", "trace path", "actual root cause", "meta-lesson")):
        return "context"
    return "open" if "pinpoint" in combined or record.source_type == "roadmap_action" else "active"


def deferral_for(record: RoadmapRecord, status: str) -> str:
    if status == "deferred_with_rationale":
        return "Deferred by roadmap/approved plan until prerequisite contracts or post-2.0 research admission gates are satisfied."
    if status == "rejected_not_claw":
        return "Rejected because the source describes clone-only breadth or behavior outside Claw's machine-truth/clawable-harness identity."
    if status == "superseded":
        return "Superseded by a newer roadmap entry or canonical Rust/control-plane contract; keep only for audit traceability."
    if status == "stale_done":
        return "Marked done in roadmap but needs freshness re-verification before being used as release evidence."
    return ""


def verification_for(record: RoadmapRecord, status: str) -> str:
    if status == "context":
        return "none_context_only"
    if status in {"done_verify", "stale_done"}:
        return "verify_existing_evidence_and_regression_guard"
    cat = category_for(f"{record.path} {record.title}")
    if cat == "docs_license":
        return "docs_snapshot_or_help_output_check"
    if cat == "windows_install":
        return "install_matrix_or_cross_platform_smoke"
    if cat == "provider":
        return "provider_routing_contract_test"
    if cat == "plugin_mcp":
        return "plugin_mcp_lifecycle_contract_test"
    if cat == "event_report":
        return "schema_golden_fixture_or_consumer_contract_test"
    if cat == "branch_recovery":
        return "git_fixture_or_recovery_recipe_test"
    if cat == "boot":
        return "worker_boot_state_machine_or_cli_json_contract_test"
    return "targeted_regression_or_acceptance_test_required"


def dependencies_for(record: RoadmapRecord, status: str) -> list[str]:
    combined = f"{record.path} {record.title}".lower()
    deps: list[str] = []
    if status == "context":
        return deps
    if "phase 2" in combined or category_for(combined) == "event_report":
        deps.append("stream_1_worker_boot_session_control")
    if "phase 3" in combined or category_for(combined) == "branch_recovery":
        deps.append("stream_2_event_reporting_contracts")
    if "phase 4" in combined or category_for(combined) == "task_policy":
        deps.append("stream_2_event_reporting_contracts")
    if "phase 5" in combined or category_for(combined) == "plugin_mcp":
        deps.append("stream_1_worker_boot_session_control")
    if any(k in combined for k in ["zed", "acp", "desktop", "marketplace"]):
        deps.append("stable_alpha_contracts")
    if any(k in combined for k in ["provider", "install", "windows", "docs", "license"]):
        deps.append("adoption_overlay_triage")
    return sorted(set(deps))


def roadmap_item(record: RoadmapRecord, index: int) -> dict[str, Any]:
    status = status_for(record)
    item_id = f"CC2-RM-{'H' if record.source_type == 'roadmap_heading' else 'A'}{index:04d}-{slugify(record.title, 40)}"
    bucket = release_bucket_for(record, status)
    return {
        "id": item_id,
        "title": record.title,
        "source_anchor": f"ROADMAP.md:L{record.line}",
        "source_type": record.source_type,
        "source_path": "ROADMAP.md",
        "source_context": record.path,
        "source_line": record.line,
        "source_level": record.level if record.source_type == "roadmap_heading" else None,
        "source_ordinal": record.ordinal,
        "release_bucket": bucket,
        "lifecycle_status": status,
        "status": status,
        "category": category_for(f"{record.path} {record.title}"),
        "owner_lane": stream_for(record),
        "dependencies": dependencies_for(record, status),
        "verification_required": verification_for(record, status),
        "deferral_rationale": deferral_for(record, status),
    }


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def issue_item(issue: dict[str, Any], source_name: str, source_type: str, bucket: str) -> dict[str, Any]:
    title = issue.get("title") or f"Issue #{issue.get('number')}"
    number = issue.get("number")
    body = f"{title} {issue.get('body') or ''}"
    status = "open" if issue.get("state", "OPEN").lower() != "closed" else "done_verify"
    return {
        "id": f"CC2-ISSUE-{source_name.upper()}-{number}",
        "title": title,
        "source_anchor": f".omx/research/{source_name}.json#issue-{number}",
        "source_type": source_type,
        "source_path": f".omx/research/{source_name}.json",
        "issue_number": number,
        "issue_url": issue.get("url"),
        "release_bucket": bucket,
        "lifecycle_status": status,
        "status": status,
        "category": category_for(body),
        "owner_lane": stream_for(RoadmapRecord(0, 0, title, title, source_type)),
        "dependencies": ["roadmap_board_triage"],
        "verification_required": "issue_acceptance_repro_or_triage_decision",
        "deferral_rationale": "Latest issue intake is admitted only when it matches freeze/admission rules; otherwise remains 2.x_intake." if bucket == "2.x_intake" else "",
    }


def repo_context_item(meta: dict[str, Any], source_name: str) -> dict[str, Any]:
    owner = meta.get("nameWithOwner", source_name)
    return {
        "id": f"CC2-PARITY-{source_name.upper()}-REPO-CONTEXT",
        "title": f"Parity source metadata: {owner}",
        "source_anchor": f".omx/research/{source_name}-repo.json",
        "source_type": "parity_repo_context",
        "source_path": f".omx/research/{source_name}-repo.json",
        "release_bucket": "context",
        "lifecycle_status": "context",
        "status": "context",
        "category": "governance",
        "owner_lane": "parity_overlay",
        "dependencies": [],
        "verification_required": "none_context_only",
        "deferral_rationale": "",
        "repo": {
            "nameWithOwner": owner,
            "url": meta.get("url"),
            "pushedAt": meta.get("pushedAt"),
            "latestRelease": meta.get("latestRelease"),
            "licenseInfo": meta.get("licenseInfo"),
        },
    }


def summarize_counts(items: list[dict[str, Any]], key: str) -> dict[str, int]:
    out: dict[str, int] = {}
    for item in items:
        out[item[key]] = out.get(item[key], 0) + 1
    return dict(sorted(out.items()))


def render_markdown(board: dict[str, Any]) -> str:
    lines = [
        "# Claw Code 2.0 Canonical Board",
        "",
        f"Generated: `{board['generated_at']}`",
        f"Roadmap SHA-256 prefix: `{board['sources']['roadmap']['sha256_prefix']}`",
        "",
        "## Summary",
        "",
        f"- Total items: **{len(board['items'])}**",
        f"- Roadmap headings covered: **{board['coverage']['roadmap_headings_total']} / {board['coverage']['roadmap_headings_mapped']}**",
        f"- Roadmap ordered actions covered: **{board['coverage']['roadmap_actions_total']} / {board['coverage']['roadmap_actions_mapped']}**",
        "",
        "### By lifecycle status",
        "",
    ]
    for status, count in board["summary"]["by_status"].items():
        lines.append(f"- `{status}`: {count}")
    lines.extend(["", "### By release bucket", ""])
    for bucket, count in board["summary"]["by_release_bucket"].items():
        lines.append(f"- `{bucket}`: {count}")
    lines.extend(["", "## Board Items", ""])
    for item in board["items"]:
        deps = ", ".join(item.get("dependencies") or []) or "none"
        rationale = item.get("deferral_rationale") or ""
        lines.extend([
            f"### {item['id']}",
            f"- Title: {item['title']}",
            f"- Source: `{item['source_anchor']}` (`{item['source_type']}`)",
            f"- Bucket/status: `{item['release_bucket']}` / `{item['status']}`",
            f"- Category/lane: `{item.get('category')}` / `{item.get('owner_lane')}`",
            f"- Dependencies: {deps}",
            f"- Verification: `{item['verification_required']}`",
            f"- Deferral rationale: {rationale}",
            "",
        ])
    return "\n".join(lines)


def validate_board(board: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    seen = set()
    for index, item in enumerate(board.get("items", []), 1):
        missing = [field for field in REQUIRED_ITEM_FIELDS if field not in item]
        if missing:
            errors.append(f"item {index} missing fields: {missing}")
        if item.get("id") in seen:
            errors.append(f"duplicate id: {item.get('id')}")
        seen.add(item.get("id"))
        if item.get("status") not in STATUSES:
            errors.append(f"{item.get('id')} invalid status {item.get('status')}")
        if item.get("release_bucket") not in RELEASE_BUCKETS:
            errors.append(f"{item.get('id')} invalid release_bucket {item.get('release_bucket')}")
        if not isinstance(item.get("dependencies"), list):
            errors.append(f"{item.get('id')} dependencies must be list")
    coverage = board.get("coverage", {})
    if coverage.get("unmapped_roadmap_heading_lines"):
        errors.append(f"unmapped heading lines: {coverage['unmapped_roadmap_heading_lines']}")
    if coverage.get("duplicate_roadmap_heading_lines"):
        errors.append(f"duplicate heading lines: {coverage['duplicate_roadmap_heading_lines']}")
    if coverage.get("roadmap_headings_total") != coverage.get("roadmap_headings_mapped"):
        errors.append("roadmap heading total/mapped mismatch")
    return errors


def build_board(repo_root: Path) -> dict[str, Any]:
    roadmap_path = repo_root / "ROADMAP.md"
    source_omx = find_source_omx(repo_root)
    research = source_omx / "research"
    plan_path = source_omx / "plans" / "claw-code-2-0-adaptive-plan.md"
    headings, actions = parse_roadmap(roadmap_path)
    items = [roadmap_item(record, i) for i, record in enumerate(headings, 1)]
    items.extend(roadmap_item(record, i) for i, record in enumerate(actions, 1))

    latest_issues = load_json(research / "claw-open-latest.json")
    all_issues = load_json(research / "claw-issues.json")
    items.extend(issue_item(issue, "claw-open-latest", "latest_open_issue", "2.x_intake") for issue in latest_issues)
    # Include a small real-issue sample from the full freeze to keep the board tied to the larger issue manifest without exploding scope.
    for issue in all_issues[:50]:
        title_body = f"{issue.get('title','')} {issue.get('body','')}".lower()
        if any(k in title_body for k in ["security", "windows", "install", "provider", "model", "session", "license", "zed", "spam", "plugin"]):
            items.append(issue_item(issue, "claw-issues", "issue_theme", "beta_adoption"))
    for source_name in ["opencode", "codex"]:
        repo_meta = load_json(research / f"{source_name}-repo.json")
        items.append(repo_context_item(repo_meta, source_name))

    heading_lines = [record.line for record in headings]
    mapped_heading_lines = [item["source_line"] for item in items if item.get("source_type") == "roadmap_heading"]
    duplicate_heading_lines = sorted(line for line in set(mapped_heading_lines) if mapped_heading_lines.count(line) != 1)
    unmapped_heading_lines = sorted(set(heading_lines) - set(mapped_heading_lines))

    board = {
        "schema_version": "cc2.board.v1",
        "generated_at": datetime.now(timezone.utc).replace(microsecond=0).isoformat(),
        "generation_policy": {
            "ultragoal_mutation": "forbidden",
            "roadmap_coverage": "all markdown headings plus top-level ordered roadmap actions",
            "status_values": sorted(STATUSES),
            "release_buckets": sorted(RELEASE_BUCKETS),
        },
        "sources": {
            "roadmap": {
                "path": "ROADMAP.md",
                "sha256_prefix": sha256_prefix(roadmap_path),
                "heading_count": len(headings),
                "ordered_action_count": len(actions),
            },
            "approved_plan": {
                "path": ".omx/plans/claw-code-2-0-adaptive-plan.md",
                "sha256_prefix": sha256_prefix(plan_path),
            },
            "research": {
                "root": str(source_omx / "research"),
                "claw_open_latest_count": len(latest_issues),
                "claw_issues_count": len(all_issues),
                "opencode_repo": ".omx/research/opencode-repo.json",
                "codex_repo": ".omx/research/codex-repo.json",
            },
        },
        "coverage": {
            "roadmap_headings_total": len(headings),
            "roadmap_headings_mapped": len(mapped_heading_lines),
            "unmapped_roadmap_heading_lines": unmapped_heading_lines,
            "duplicate_roadmap_heading_lines": duplicate_heading_lines,
            "roadmap_actions_total": len(actions),
            "roadmap_actions_mapped": len([item for item in items if item.get("source_type") == "roadmap_action"]),
        },
        "summary": {},
        "items": items,
    }
    board["summary"] = {
        "by_status": summarize_counts(items, "status"),
        "by_release_bucket": summarize_counts(items, "release_bucket"),
        "by_source_type": summarize_counts(items, "source_type"),
        "by_owner_lane": summarize_counts(items, "owner_lane"),
    }
    errors = validate_board(board)
    if errors:
        raise SystemExit("board validation failed:\n" + "\n".join(errors))
    return board


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--out-dir", type=Path, default=None)
    args = parser.parse_args()
    repo_root = args.repo_root.resolve()
    out_dir = args.out_dir or (repo_root / ".omx" / "cc2")
    try:
        board = build_board(repo_root)
    except FileNotFoundError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    out_dir.mkdir(parents=True, exist_ok=True)
    board_json = out_dir / "board.json"
    board_md = out_dir / "board.md"
    board_json.write_text(json.dumps(board, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    renderer = repo_root / ".omx" / "cc2" / "render_board_md.py"
    if renderer.exists():
        subprocess.run([sys.executable, str(renderer), str(board_json), str(board_md)], check=True, cwd=str(repo_root))
    else:
        board_md.write_text(render_markdown(board) + "\n", encoding="utf-8")

    print(f"wrote {board_json}")
    print(f"wrote {board_md}")
    print(f"roadmap headings mapped: {board['coverage']['roadmap_headings_mapped']}/{board['coverage']['roadmap_headings_total']}")
    print(f"roadmap actions mapped: {board['coverage']['roadmap_actions_mapped']}/{board['coverage']['roadmap_actions_total']}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
