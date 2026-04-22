# Fix-Locus #164 — JSON Envelope Contract Migration

**Status:** 📋 Proposed (2026-04-23, cycle #77). Updated cycle #85 (2026-04-23) with v1.5 baseline phase after fresh-dogfood discovery (#168) proved v1.0 was never coherent.

**Class:** Contract migration (not a patch). Affects EVERY `--output-format json` command.

**Bundle:** Typed-error family — joins #102 + #121 + #127 + #129 + #130 + #245 + **#164**. Contract-level implementation of §4.44 typed-error envelope.

---

## 0. CRITICAL UPDATE (Cycle #85 via #168 Evidence)

**Premise revision:** This locus document originally framed the problem as **"v1.0 (incoherent) → v2.0 (target schema)"** migration. **Fresh-dogfood validation in cycle #84 proved this framing was underspecified.**

**Actual problem (evidence from #168):**

- There is **no coherent v1.0 envelope contract**. Each verb has a bespoke JSON shape.
- `claw list-sessions --output-format json` emits `{command, sessions}` — has `command` field
- `claw doctor --output-format json` emits `{checks, kind, message, ...}` — no `command` field
- `claw bootstrap hello --output-format json` emits **NOTHING** (silent failure with exit 0)
- Each verb renderer was written independently with no coordinating contract

**Revised migration plan — three phases instead of two:**

1. **Phase 0 (Emergency):** Fix silent failures (#168 bootstrap JSON). Every `--output-format json` command must emit valid JSON.
2. **Phase 1 (v1.5 Baseline):** Establish minimal JSON invariants across all 14 verbs without breaking existing consumers:
   - Every command emits valid JSON when `--output-format json` is passed
   - Every command has a top-level `kind` field identifying the verb
   - Every error envelope follows the confirmed `{error, hint, kind, type}` shape
   - Every success envelope has the verb name in a predictable location
   - **Effort:** ~3 dev-days (no new design, just fill gaps and normalize bugs)
3. **Phase 2 (v2.0 Wrapped Envelope):** Execute the original Phase 1 plan documented below — common metadata wrapper, nested data/error objects, opt-in via `--envelope-version=2.0`.
4. **Phase 3 (v2.0 Default):** Original Phase 2 plan below.
5. **Phase 4 (v1.0/v1.5 Deprecation):** Original Phase 3 plan below.

**Why add Phase 0 + Phase 1 (v1.5)?**

- You can't migrate from "incoherent" to "coherent v2.0" in one jump. Intermediate coherence (v1.5 baseline) is required.
- Consumer code built against "whatever v1 emits today" needs a stable target to transition from.
- **Silent failures (bootstrap JSON) must be fixed BEFORE any migration** — otherwise consumers have no way to detect breakage.

**Blocker resolved:** The original blocker "v1.0 design vs v2.0 design" is actually "no v1 design exists; let's make one (v1.5) then migrate." This is a **clearer, lower-risk migration path**.

**Revised effort estimate:** ~9 dev-days total (Phase 0: 1 day + Phase 1/v1.5: 3 days + Phase 2/v2.0: 5 days) instead of ~6 dev-days for a direct v1.0→v2.0 migration (which would have failed given the incoherent baseline).

**Doctrine implication:** Cycles #76–#82 diagnosed "aspirational vs current" correctly but missed that "current" was never a single thing. Cycle #84 fresh-dogfood caught this. **Fresh-dogfood discipline (principle #9) prevented a 6-day migration effort from hitting an unsolvable baseline problem.**

---

## 1. Scope — What This Migration Affects

**Every JSON-emitting verb.** Audit across the 14 documented verbs:

| Verb | Current top-level keys | Schema-conformant? |
|---|---|---|
| `doctor` | checks, has_failures, **kind**, message, report, summary | ❌ No (kind=verb-id, flat) |
| `status` | config_load_error, **kind**, model, ..., workspace | ❌ No |
| `version` | git_sha, **kind**, message, target, version | ❌ No |
| `sandbox` | active, ..., **kind**, ...supported | ❌ No |
| `help` | **kind**, message | ❌ No (minimal) |
| `agents` | action, agents, count, **kind**, summary, working_directory | ❌ No |
| `mcp` | action, config_load_error, ..., **kind**, servers | ❌ No |
| `skills` | action, **kind**, skills, summary | ❌ No |
| `system-prompt` | **kind**, message, sections | ❌ No |
| `dump-manifests` | error, hint, **kind**, type | ❌ No (emits error envelope for success) |
| `bootstrap-plan` | **kind**, phases | ❌ No |
| `acp` | aliases, ..., **kind**, ...tracking | ❌ No |
| `export` | file, **kind**, markdown, messages, session_id | ❌ No |
| `state` | error, hint, **kind**, type | ❌ No (emits error envelope for success) |

**All 14 verbs diverge from SCHEMAS.md.** The gap is 100%, not a partial drift.

---

## 2. The Two Envelope Shapes

### 2a. Current Binary Shape (Flat Top-Level)

```json
// Success example (claw doctor --output-format json)
{
  "kind": "doctor",          // verb identity
  "checks": [...],
  "summary": {...},
  "has_failures": false,
  "report": "...",
  "message": "..."
}

// Error example (claw doctor foo --output-format json)
{
  "error": "unrecognized argument...",   // string, not object
  "hint": "Run `claw --help` for usage.",
  "kind": "cli_parse",        // error classification (overloaded)
  "type": "error"             // not in schema
}
```

**Properties:**
- Flat top-level
- `kind` field is **overloaded** (verb-id in success, error-class in error)
- No common wrapper metadata (timestamp, exit_code, schema_version)
- `error` is a string, not a structured object

### 2b. Documented Schema Shape (Nested, Wrapped)

```json
// Success example (per SCHEMAS.md)
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "doctor",
  "exit_code": 0,
  "output_format": "json",
  "schema_version": "1.0",
  "data": {
    "checks": [...],
    "summary": {...},
    "has_failures": false
  }
}

// Error example (per SCHEMAS.md)
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "doctor",
  "exit_code": 1,
  "output_format": "json",
  "schema_version": "1.0",
  "error": {
    "kind": "parse",           // enum, nested
    "operation": "parse_args",
    "target": "subcommand `doctor`",
    "retryable": false,
    "message": "unrecognized argument...",
    "hint": "Run `claw --help` for usage."
  }
}
```

**Properties:**
- Common metadata wrapper (timestamp, command, exit_code, output_format, schema_version)
- `data` (payload) vs. `error` (failure) as **sibling fields**, never coexisting
- `kind` in error is the enum from §4.44 (filesystem/auth/session/parse/runtime/mcp/delivery/usage/policy/unknown)
- `error` is a structured object with operation/target/retryable

---

## 3. Migration Strategy — Phased Rollout

**Principle:** Don't break downstream consumers mid-migration. Support both shapes during overlap, then deprecate.

### Phase 1 — Dual-Envelope Mode (Opt-In)

**Deliverables:**
- New flag: `--envelope-version=2.0` (or `--schema-version=2.0`)
- When flag set: emit new (schema-conformant) envelope
- When flag absent: emit current (flat) envelope
- SCHEMAS.md: add "Legacy (v1.0)" section documenting current flat shape alongside v2.0

**Implementation:**
- Single `envelope_version` parameter in `CliOutputFormat` enum
- Every verb's JSON writer checks version, branches accordingly
- Shared wrapper helper: `wrap_v2(payload, command, exit_code)`

**Consumer impact:** Opt-in. Existing consumers unchanged. New consumers can opt in.

**Timeline estimate:** ~2 days for 14 verbs + shared wrapper + tests.

### Phase 2 — Default Version Bump

**Deliverables:**
- Default changes from v1.0 → v2.0
- New flag: `--legacy-envelope` to opt back into flat shape
- Migration guide added to SCHEMAS.md and CHANGELOG
- Release notes: "Breaking change in envelope, pre-migration opt-in available via --legacy-envelope"

**Consumer impact:** Existing consumers must add `--legacy-envelope` OR update to v2.0 schema. Grace period = "until Phase 3."

**Timeline estimate:** Immediately after Phase 1 ships.

### Phase 3 — Flat-Shape Deprecation

**Deliverables:**
- `--legacy-envelope` flag prints deprecation warning to stderr
- SCHEMAS.md "Legacy v1.0" section marked DEPRECATED
- v3.0 release (future): remove flag entirely, binary only emits v2.0

**Consumer impact:** Full migration required by v3.0.

**Timeline estimate:** Phase 3 after ~6 months of Phase 2 usage.

---

## 4. Implementation Details

### 4a. Shared Wrapper Helper

```rust
// rust/crates/rusty-claude-cli/src/json_envelope.rs (new file)

pub fn wrap_v2_success<T: Serialize>(command: &str, data: T) -> Value {
    serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "command": command,
        "exit_code": 0,
        "output_format": "json",
        "schema_version": "2.0",
        "data": data,
    })
}

pub fn wrap_v2_error(command: &str, error: StructuredError) -> Value {
    serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "command": command,
        "exit_code": 1,
        "output_format": "json",
        "schema_version": "2.0",
        "error": {
            "kind": error.kind,
            "operation": error.operation,
            "target": error.target,
            "retryable": error.retryable,
            "message": error.message,
            "hint": error.hint,
        },
    })
}

pub struct StructuredError {
    pub kind: &'static str,   // enum from §4.44
    pub operation: String,
    pub target: String,
    pub retryable: bool,
    pub message: String,
    pub hint: Option<String>,
}
```

### 4b. Per-Verb Migration Pattern

```rust
// Before (current flat shape):
match output_format {
    CliOutputFormat::Json => {
        serde_json::to_string_pretty(&DoctorOutput {
            kind: "doctor",
            checks,
            summary,
            has_failures,
            message,
            report,
        })
    }
    CliOutputFormat::Text => render_text(&data),
}

// After (v2.0 with v1.0 fallback):
match (output_format, envelope_version) {
    (CliOutputFormat::Json, 2) => {
        json_envelope::wrap_v2_success("doctor", DoctorData { checks, summary, has_failures })
    }
    (CliOutputFormat::Json, 1) => {
        // Legacy flat shape (with deprecation warning at Phase 3)
        serde_json::to_value(&LegacyDoctorOutput { kind: "doctor", ...})
    }
    (CliOutputFormat::Text, _) => render_text(&data),
}
```

### 4c. Error Classification Migration

Current error `kind` values (found in binary):
- `cli_parse`, `no_managed_sessions`, `unknown`, `missing_credentials`, `session_not_found`

Target v2.0 enum (per §4.44):
- `filesystem`, `auth`, `session`, `parse`, `runtime`, `mcp`, `delivery`, `usage`, `policy`, `unknown`

**Migration table:**
| Current kind | v2.0 error.kind |
|---|---|
| `cli_parse` | `parse` |
| `no_managed_sessions` | `session` (with operation: "list_sessions") |
| `missing_credentials` | `auth` |
| `session_not_found` | `session` (with operation: "resolve_session") |
| `unknown` | `unknown` |

---

## 5. Acceptance Criteria

1. **Schema parity:** Every `--output-format json` command emits v2.0 envelope shape exactly per SCHEMAS.md
2. **Success/error symmetry:** Success envelopes have `data` field; error envelopes have `error` object; never both
3. **kind semantic unification:** `data.kind` = verb identity (when present); `error.kind` = enum from §4.44. No overloading.
4. **Common metadata:** `timestamp`, `command`, `exit_code`, `output_format`, `schema_version` present in ALL envelopes
5. **Dual-mode support:** `--envelope-version=1|2` flag allows opt-in/opt-out during migration
6. **Tests:** Per-verb golden test fixtures for both v1.0 and v2.0 envelopes
7. **Documentation:** SCHEMAS.md documents both versions with deprecation timeline

---

## 6. Risks

### 6a. Breaking Change Risk

Phase 2 (default version bump) WILL break consumers that depend on flat-shape envelope. Mitigations:
- Dual-mode flag allows opt-in testing before default change
- Long grace period (Phase 3 deprecation ~6 months post-Phase 2)
- Clear migration guide + example consumer code

### 6b. Implementation Risk

14 verbs to migrate. Each verb has its own success shape (`checks`, `agents`, `phases`, etc.). Payload structure stays the same; only the wrapper changes. Mechanical but high-volume.

**Estimated diff size:** ~200 lines per verb × 14 verbs = ~2,800 lines (mostly boilerplate).

**Mitigation:** Start with doctor, status, version as pilot. If pattern works, batch remaining 11.

### 6c. Error Classification Remapping Risk

Changing `kind: "cli_parse"` to `error.kind: "parse"` is a breaking change even within the error envelope. Consumers doing `response["kind"] == "cli_parse"` will break.

**Mitigation:** Document explicitly in migration guide. Provide sed script if needed.

---

## 7. Deliverables Summary

| Item | Phase | Effort |
|---|---|---|
| `json_envelope.rs` shared helper | Phase 1 | 1 day |
| 14 verb migrations (pilot 3 + batch 11) | Phase 1 | 2 days |
| `--envelope-version` flag | Phase 1 | 0.5 day |
| Dual-mode tests (golden fixtures) | Phase 1 | 1 day |
| SCHEMAS.md updates (v1.0 + v2.0) | Phase 1 | 0.5 day |
| Default version bump | Phase 2 | 0.5 day |
| Deprecation warnings | Phase 3 | 0.5 day |
| Migration guide doc | Phase 1 | 0.5 day |

**Total estimate:** ~6 developer-days for Phase 1 (the core work). Phases 2/3 are cheap follow-ups.

---

## 8. Rollout Timeline (Proposed)

- **Week 1:** Phase 1 — dual-mode support + pilot migration (3 verbs)
- **Week 2:** Phase 1 completion — remaining 11 verbs + full test coverage
- **Week 3:** Stabilization period, gather consumer feedback
- **Month 2:** Phase 2 — default version bump
- **Month 8:** Phase 3 — deprecation warnings
- **v3.0 release:** Remove `--legacy-envelope` flag, v1.0 shape no longer supported

---

## 9. Related

- **ROADMAP #164:** The originating pinpoint (this document is its fix-locus)
- **ROADMAP §4.44:** Typed-error contract (defines the error.kind enum this migration uses)
- **SCHEMAS.md:** The envelope schema this migration makes reality
- **Typed-error family:** #102, #121, #127, #129, #130, #245, **#164**

---

**Cycle #77 locus doc. Ready for author review + pilot implementation decision.**
