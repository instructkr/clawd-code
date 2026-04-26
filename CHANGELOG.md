# Changelog

All notable changes to claw-code are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (currently pre-1.0).

## [Unreleased] — 2026-04-26 to 2026-04-27 (extended dogfood audit cycles)

Branch: `feat/jobdori-168c-emission-routing`

### Added — Documentation

- **CHANGELOG.md** — This file (cycle #424)
- **docs/PINPOINT_FILING_GUIDE.md** — Step-by-step pinpoint filing workflow with #290 worked example (cycle #422)
- **docs/SUPPORTED_PROVIDERS.md** — Documents 4 providers (Anthropic, xAI, DashScope/Qwen/Kimi, OpenAI/compat) from MODEL_REGISTRY (cycle #420)
- **TROUBLESHOOTING.md** — Operational guidance for 5 critical failure modes (#286, #287, #289, #290, #291) (cycles #418, #423)
- **ROADMAP.md Pinpoint Cluster Index** — Navigation aid for 8 named clusters (cycle #421)
- **ROADMAP.md Extended Dogfood Audit Summary** — Cycles #388-#415 overview (cycle #416)
- **README.md Contributing section** — Unified navigation to SECURITY/ROADMAP/CONTRIBUTING/ISSUE_TEMPLATE (cycle #415)
- **SECURITY.md** — Responsible-disclosure stub with reporting via GitHub Security Advisories (cycle #414)
- **CONTRIBUTING.md** — Codifies pinpoint filing format, build commands, branch naming (cycle #411)
- **.github/ISSUE_TEMPLATE/pinpoint.md** — Discoverable canonical issue template (cycle #412)
- **LICENSE** — Root MIT license file (cycle #410)

### Fixed — Code

- **#256** — Anthropic tool-result request ordering (pre-audit)
- **#122b** — `claw doctor` broad-path warning
- **#160** — Reserved-semantic-verb slash-command guidance

### Filed — Pinpoints (ROADMAP.md)

46 pinpoints filed (#241-#291) during extended dogfood audit. New clusters identified:
- **Auto-compaction (4-deep):** #283, #287 (CRITICAL), #288, #289
- **Transport / Provider Resilience:** #266, #285, #290, #291
- **Provider Infrastructure:** #245, #246, #285
- **Tool Lifecycle / Hooks:** #254, #268, #274, #280, #286
- **CLI Dispatch:** #262, #267, #272, #282, #283
- **Persistence / Migration:** #278, #279
- **Provenance Consolidation:** #259, #271, #273, #275
- **Slash-command Contract:** #284

See [ROADMAP.md](./ROADMAP.md#pinpoint-cluster-index) for full list.

### Live evidence integrated

- @Sigrid Jin: license verification, ultraplan functionality, provider-config source-of-truth → pinpoints #284, #285
- gaebal-gajae sustained `500 empty_stream` (11+ incidents in 3hr+) → pinpoints #290, #291

---

## Process

This release demonstrates the pinpoint-driven workflow:
1. **Identify friction** during real claw-code usage
2. **File pinpoint** to ROADMAP.md with canonical 5-section format
3. **Ship docs/code fix** when concrete delta is small
4. **Cluster pinpoints** to expose architectural patterns
5. **Document mitigations** in TROUBLESHOOTING.md

See [docs/PINPOINT_FILING_GUIDE.md](./docs/PINPOINT_FILING_GUIDE.md) for details.
