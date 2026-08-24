#!/usr/bin/env python3
"""Validate the worker-2 CC2 issue/parity intake fragment."""
from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
INTAKE = ROOT / ".omx" / "cc2" / "issue-parity-intake.json"
REQUIRED_ISSUES = set(range(3028, 3039)) | {3007, 3006, 3020, 3005, 3003, 2997, 3023, 3004}
ALLOWED_STATUS = {
    "context",
    "active",
    "open",
    "done_verify",
    "stale_done",
    "superseded",
    "deferred_with_rationale",
    "rejected_not_claw",
}
ALLOWED_BUCKETS = {"alpha_blocker", "beta_adoption", "ga_ecosystem", "post_2_0_research"}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"FAIL: {message}")


def main() -> None:
    data = json.loads(INTAKE.read_text())
    issue_rows = data.get("issue_clusters", [])
    parity_rows = data.get("parity_rows", [])

    seen = {row.get("source_number") for row in issue_rows}
    missing = sorted(REQUIRED_ISSUES - seen)
    extra = sorted(seen - REQUIRED_ISSUES)
    require(not missing, f"missing required issue rows: {missing}")
    require(not extra, f"unexpected issue rows in scoped intake: {extra}")
    require(len(issue_rows) == len(REQUIRED_ISSUES), "duplicate or missing issue row count")

    ids = [row.get("id") for row in issue_rows + parity_rows]
    require(len(ids) == len(set(ids)), "duplicate ids present")

    for row in issue_rows + parity_rows:
        row_id = row.get("id")
        for field in ["source_anchor", "source_type", "release_bucket", "lifecycle_status", "dependencies", "verification_required"]:
            require(row.get(field) not in (None, "", []), f"{row_id} missing {field}")
        require(row["release_bucket"] in ALLOWED_BUCKETS, f"{row_id} invalid release_bucket {row['release_bucket']}")
        require(row["lifecycle_status"] in ALLOWED_STATUS, f"{row_id} invalid lifecycle_status {row['lifecycle_status']}")
        if row["lifecycle_status"] == "deferred_with_rationale":
            require(row.get("deferral_rationale"), f"{row_id} deferred without rationale")

    require(len(parity_rows) >= data["coverage"]["parity_rows_expected_minimum"], "not enough parity rows")
    print(f"PASS issue/parity intake: {len(issue_rows)} issue rows, {len(parity_rows)} parity rows")


if __name__ == "__main__":
    main()
