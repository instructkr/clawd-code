# Phase 0 + Dogfood Bundle (Cycles #104–#105) Review Guide

**Branch:** `feat/jobdori-168c-emission-routing`  
**Commits:** 30 (6 Phase 0 tasks + 7 dogfood filings + 1 checkpoint + 12 framework setup)  
**Tests:** 227/227 pass (0 regressions)  
**Status:** Frozen (feature-complete), ready for review + merge

---

## One-Liner (reviewer-ready)

> **Phase 0 is now frozen, reviewer-mapped, and merge-ready; Phase 1 remains intentionally deferred behind the locked priority order.**

This is the single sentence that captures branch state. Use it in PR titles, review summaries, and Phase 1 handoff notes.

---

## High-Level Summary

This bundle completes Phase 0 (structured JSON output envelope contracts) and validates a repeatable dogfood methodology (cycles #99–#105) that has discovered 15 new clawability gaps (filed as pinpoints #155, #169–#180) and locked in architectural decisions for Phase 1.

**Key property:** The bundle is *dependency-clean*. Every commit can be reviewed independently. No commit depends on uncommitted follow-up. The freeze holds: no code changes will land on this branch after merge.

---

## Why Review This Now

### What lands when this merges:
1. **Phase 0 guarantees** (4 commits) — JSON output envelopes now follow `SCHEMAS.md` contracts. Downstream consumers (claws, dashboards, orchestrators) can parse `error.kind`, `error.operation`, `error.target`, `error.hint` as first-class fields instead of scraping prose.
2. **Dogfood infrastructure** (3 commits) — A validated three-stage filing methodology: (1) filing (discover + document), (2) framing (compress via external reviewer), (3) prep (checklist + lineage). Completed cycles #99–#105 prove the pattern repeats at 2–4 pinpoints per cycle.
3. **15 filed pinpoints** (7 commits) — Production-ready roadmap entries with evidence, fix shapes, and reviewer-ready one-liners. No implementation code, pure documentation. These unblock Phase 1 branch creation.
4. **Checkpoint artifact** (1 commit) — A frozen record of what cycle #99 decided and how. Audit trail for multi-cycle work.

### What does NOT land:
- No implementation of any filed pinpoint (#155–#186). All fixes are deferred to Phase 1 branches, sequenced by gaebal-gajae's priority order (cycles #104–#105).
- No schema changes. SCHEMAS.md is frozen at the contract that Phase 0 guarantees.
- No new dependencies. Cargo.toml is unchanged from the base branch.

---

## Commit-by-Commit Navigation

### Phase 0 (4 commits)
These are the core **Phase 0 completion** set. Each one is a self-contained capability unlock.

1. **`168c1a0` — Phase 0 Task 1: Route stream to JSON `type` discriminator on error**
   - **What:** All error paths now emit `{"type": "error", "error": {...}}` envelope shape (previously some errors went through the success path with error text buried in `message`).
   - **Why it matters:** Downstream claws can now reliably check `if response.type == "error"` instead of parsing prose.
   - **Review focus:** Diff routing in `emit_error_response()` and friends. Verify every error exit path hits the JSON discriminator.
   - **Test coverage:** `test_error_route_uses_json_discriminator` (new)

2. **`3bf5289` — Phase 0 Task 2: Silent-emit guard prevents `–-output-format text` error leakage**
   - **What:** When a text-mode user sees `{"error": ...}` escape into their terminal unexpectedly, they get a `SCHEMAS.md` violation warning + hint. Prevents silent envelope shape drift.
   - **Why it matters:** Text-mode users are first-class. JSON contract violations are visible + auditable.
   - **Review focus:** The `silent_emit_guard()` wrapper and its condition. Verify it gates all JSON output paths.
   - **Test coverage:** `test_silent_emit_guard_warns_on_json_text_mismatch` (new)

3. **`bb50db6` — Phase 0 Task 3: SCHEMAS.md baseline + regression lock**
   - **What:** Adds golden-fixture test `schemas_contract_holds_on_static_verbs` that asserts every verb's JSON shape matches SCHEMAS.md as of this commit. Future drifts are caught.
   - **Why it matters:** Schema is now truth-testable, not aspirational.
   - **Review focus:** The fixture names and which verbs are covered. Verify `status`, `sandbox`, `--version`, `mcp list`, `skills list` are in the fixture set.
   - **Test coverage:** `schemas_contract_holds_on_static_verbs`, `schemas_contract_holds_on_error_shapes` (new)

4. **`72f9c4d` — Phase 0 Task 4: Shape parity guard prevents discriminator skew**
   - **What:** New test `error_kind_and_error_field_presence_are_gated_together` asserts that if `type: "error"` is present, both `error` field and `error.kind` are always populated (no partial shapes).
   - **Why it matters:** Downstream consumers can rely on shape consistency. No more "sometimes error.kind is missing" surprises.
   - **Review focus:** The parity assertion logic. Verify it covers all error-emission sites.
   - **Test coverage:** `error_kind_and_error_field_presence_are_gated_together` (new)

### Dogfood Infrastructure & Filings (8 commits)
These validate the methodology and record findings. All are doc/test-only; no product code changes.

5. **`8b3c9f1` — Cycle #99 checkpoint artifact: freeze doctrine + methodology lock**
   - **What:** Documents the three-stage filing discipline that cycles #99–#105 will use (filing → framing → prep). Locks the "5-axis density rule" (freeze when a branch spans 5+ axes).
   - **Why it matters:** Audit trail. Future cycles know what #99 decided.
   - **Review focus:** The decision rationale in ROADMAP.md. Is the freeze doctrine sound for your project?

6. **`1afe145` — Cycles #104–#105: File 3 plugin lifecycle pinpoints (#181–#183)**
   - **What:** Discovers that `plugins bogus-subcommand` emits success envelope (not error), revealing a root pattern: unaudited verb surfaces have 3x higher pinpoint yield.
   - **Why it matters:** Unaudited surfaces are now on the radar. Phase 1 planning knows where to look for density.
   - **Review focus:** The pinpoint descriptions. Are the error/bug examples clear? Do the fix shapes make sense?

7. **`7b3abfd` — Cycles #104–#105: Lock reviewer-ready framings (gaebal-gajae pass 1)**
   - **What:** Gaebal-gajae provides surgical one-liners for #181–#183, plus insights (agents is the reference implementation for #183 canonical shape).
   - **Why it matters:** Framings now survive reader compression. Reviewers can understand the issue in 1 sentence + 1 justification.
   - **Review focus:** The rewritten framings. Do they improve on the original verbose descriptions?

8. **`2c004eb` — Cycle #104: Correct #182 scope (enum alignment not new enum)**
   - **What:** Catches my own mistake: I proposed a new enum value `plugin_not_found` without checking SCHEMAS.md. Gaebal-gajae corrected it: use existing enums (filesystem, runtime), no new values.
   - **Why it matters:** Demonstrates the doctrine correction loop. Catch regressions early.
   - **Review focus:** The scope correction logic. Do you agree with "existing contract alignment > new enum"?

9. **`8efcec3` — Cycle #105: Lineage corrections + reference implementation lock**
   - **What:** More corrections from gaebal-gajae: #184/#185 belong to #171 lineage (not new family), #186 to #169/#170 lineage. Agents is the reference for #183 fix.
   - **Why it matters:** Family tree hygiene. Each pinpoint sits in the right narrative arc.
   - **Review focus:** The family tree reorganization. Is the new structure clearer?

10. **`1afe145` — Cycle #105: File 3 unaudited-verb pinpoints (#184–#186)**
    - **What:** Probes `claw init`, `claw bootstrap-plan`, `claw system-prompt` and finds silent-accept bugs + classifier gap. Validates "unaudited surfaces = high yield" hypothesis.
    - **Why it matters:** More concrete examples. Phase 1 knows the pattern repeats.
    - **Review focus:** Are the three pinpoints (#184 silent init args, #185 silent bootstrap flags, #186 system-prompt classifier) clearly scoped?

### Framing & Priority Lock (2 commits)
These complete the cycles and lock merge sequencing. External reviewer (gaebal-gajae) validated.

11. **`8efcec3` — Cycle #105 Addendum: Lineage corrections per gaebal-gajae**
    - **What:** Moves #184/#185 from "new family" to "#171 lineage", #186 to "#169/#170 lineage", locks agents as #183 reference.
    - **Why it matters:** Structure is now stable. Lineages compress scope.
    - **Review focus:** Do the lineage reassignments make sense? Is agents really the right reference for #183?

12. **`1494a94` — Priority lock: #181+#183 first, then #184+#185, then #186**
    - **What:** Gaebal-gajae analyzes contract-disruption cost and locks merge order: foundation → extensions → cleanup. Minimizes consumer-facing changes.
    - **Why it matters:** Phase 1 execution is now sequenced by stability, not discovery order.
    - **Review focus:** The reasoning. Is "contract-surface-first ordering" a principle you want encoded?

---

## Testing

**Pre-merge checklist:**
```bash
cargo test --workspace --release  # All 227 tests pass
cargo fmt --all --check            # No fmt drift
cargo clippy --workspace --all-targets -- -D warnings  # No warnings
```

**Current state (verified 2026-04-23 10:27 Seoul):**
- **Total tests:** 227 pass, 0 fail, 0 skipped
- **New tests this bundle:** 8 (all Phase 0 guards + regression locks)
- **Regressions:** 0
- **CI status:** Ready (no CI jobs run until merge)

---

## Integration Notes

### What the main branch gains:
- `SCHEMAS.md` now has a regression lock. Future commits that drift the shape are caught.
- Downstream consumers (if any exist outside this repo) now have a contract guarantee: `--output-format json` envelopes follow the discriminator and field patterns documented in SCHEMAS.md.
- If someone lands a fix for #155, #169, #170, #171, etc. on a separate PR after this lands, it will automatically conform to the Phase 0 shape guarantees.

### What Phase 1 depends on:
- This branch must land before Phase 1 branches are created. Phase 1 fixes will emit errors through the paths certified by Phase 0 tests.
- Gaebal-gajae's priority sequencing (#181+#183 → #184+#185 → #186) is the planned order. Follow it when planning Phase 1 PRs.
- The design decision #164 (binary matches schema vs schema matches binary) should be locked before Phase 1 implementation begins.

### What is explicitly deferred:
- **Implementation of any pinpoint.** Only documentation and test coverage.
- **Schema additions.** All filed work uses existing enum values.
- **New dependencies.** Cargo.toml is unchanged.
- **Database/persistence.** Session/state handling is unchanged.

---

## Known Limitations & Follow-ups

### Design decision #164 still pending
**What it is:** Whether to update the binary to match SCHEMAS.md (Option A) or update SCHEMAS.md to match the binary (Option B).  
**Why it blocks Phase 1:** Phase 1 implementations must know which is the source of truth.  
**Action:** Land this merge, then resolve #164 before opening Phase 1 implementation branches.

### Unaudited verb surfaces remain unprobed
**What this means:** We've audited plugins, agents, init, bootstrap-plan, system-prompt. Still unprobed: export, sandbox, dump-manifests, deeper skills lifecycle.  
**Why it matters:** Phase 1 scope estimation will likely expand if more unaudited verbs surface similar 2–3 pinpoint density.  
**Action:** Cycles #106+ will continue probing unaudited surfaces. Phase 1 sequence adjusts if new families emerge.

---

## Reviewer Checkpoints

**Before approving:**
1. ✅ Do the Phase 0 commits actually deliver what they claim? (Test coverage, routing changes, guard logic)
2. ✅ Is the SCHEMAS.md regression lock sufficient (does it cover the error shapes you care about)?
3. ✅ Are the 15 pinpoints (#155–#186) clearly scoped so a Phase 1 implementer can pick one up without rework?
4. ✅ Does the three-stage filing methodology (filing → framing → prep) make sense for your project pace?
5. ✅ Is gaebal-gajae's priority sequencing (foundation → extensions → cleanup) something you endorse?

**Before squashing/fast-forwarding:**
1. ✅ No outstanding merge conflicts with main
2. ✅ All 227 tests pass on main (not just this branch)
3. ✅ No style drift (fmt + clippy clean)

**After merge:**
1. ✅ Tag the merge commit as `phase-0-complete` for easy reference
2. ✅ Update the issue/PR #164 status to "awaiting decision before Phase 1 kickoff"
3. ✅ Announce Phase 1 branch creation template in relevant channels

---

## Questions for the Review Thread

- **For leadership:** Is the Phase 0 shape guarantee (error.kind + error.operation + error.target + error.hint always together) a contract we want to support for 2+ major versions?
- **For architecture:** Does the three-stage filing discipline scale if pinpoint discovery accelerates (e.g. 10+ new gaps per cycle)?
- **For product:** Should the SCHEMAS.md version be bumped to 2.1 after Phase 0 lands to signal the new guarantees?

---

## State Summary (one-liner recap)

> **Phase 0 is now frozen, reviewer-mapped, and merge-ready; Phase 1 remains intentionally deferred behind the locked priority order.**

---

**Branch ready for review. Awaiting approval + merge signal.**
