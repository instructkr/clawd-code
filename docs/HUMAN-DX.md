# Human Experience Design

How humans review, approve, and orchestrate agent work in Claw Code.

---

## Design Philosophy

Agents do the work. Humans provide judgment. The system must make it easy for humans to:

1. **Understand** what an agent did (without reading logs)
2. **Verify** that it works (without setting up environments)
3. **Approve or reject** changes (without wrestling with git)
4. **Orchestrate** high-level plans (without micromanaging each step)

---

## 1. Review Workflow

### The problem today

Agent outputs are walls of text. A human reviewing an agent's work has to:
- Scroll through hundreds of lines of tool output
- Manually diff files to understand what changed
- Guess whether the tests actually passed
- Reconstruct the agent's reasoning from scattered messages

### The target experience

**Every agent action produces a structured review artifact:**

```
┌─ Change Report ──────────────────────────────────────┐
│                                                        │
│  Agent: coder-01                                       │
│  Task: "Add input validation to User.create()"         │
│  Status: COMPLETE                                      │
│  Risk: MEDIUM                                          │
│                                                        │
│  Summary:                                              │
│  Added email format validation and password length     │
│  checks to User.create(). Also added 3 unit tests.     │
│                                                        │
│  Files changed: 3                                      │
│  ┌──────────────────────────────────────────────────┐  │
│  │ M  src/models/user.rs    +24 -2                  │  │
│  │ A  tests/user_validation.rs  +87                 │  │
│  │ M  src/errors.rs         +8                      │  │
│  └──────────────────────────────────────────────────┘  │
│                                                        │
│  Tests: 3/3 passed                                     │
│  Tokens: 12,400 (est. $0.04)                           │
│  Duration: 2m 14s                                      │
│                                                        │
│  [Approve] [Reject] [Request Changes] [View Diff]      │
└────────────────────────────────────────────────────────┘
```

### Change risk classification

Each change is automatically classified:

| Risk | Criteria | Human action |
|------|----------|-------------|
| **Low** | Tests only, documentation, formatting | Auto-approve (configurable) |
| **Medium** | New code with tests, refactoring | Review summary, optional diff |
| **High** | Auth changes, database migrations, API contract changes | Full diff review required |

### Approval flow

```
Agent completes task
  → Generate change report
  → Classify risk level
  → If LOW + auto-approve enabled: merge
  → If MEDIUM: queue for review
  → If HIGH: notify human immediately
  → Human reviews via: TUI, email, chat, or mobile
  → Approve → merge
  → Reject → agent receives feedback and retries
  → Request changes → agent addresses specific feedback
```

---

## 2. Notification & Delivery

### Design goal

Humans should never need to actively check on agents. The system pushes information at the right granularity to the right channel.

### Channel strategy

| Channel | When | Format |
|---------|------|--------|
| **Email** | Phase complete, milestone reached | HTML report with summary, diff stats, test results, preview link |
| **Chat (Slack/Discord)** | Agent needs input, task complete, error | Rich embed with approve/reject buttons |
| **Mobile push** | Agent blocked, approval required, critical error | Brief notification with deep link to review |
| **Web dashboard** | Always available | Real-time agent status, change timeline, active previews |

### Email report example

```
Subject: [Claw] Phase 2 Complete — "Add authentication" (3/5 phases done)

Phase 2: Add authentication
  Status: COMPLETE ✓
  Agent: coder-01
  Duration: 8m 32s
  Tokens: 45,200 (est. $0.14)

Changes:
  M  src/middleware/auth.rs    +142 -8
  A  src/models/session.rs     +89
  M  src/routes/login.rs       +34 -12
  A  tests/auth_tests.rs       +203

Tests: 12/12 passed
Risk: HIGH (auth changes)

Preview: https://claw.preview/abc123 (expires in 24h)
         ↑ Live deployment — click to verify

[Approve Changes] [View Full Diff] [Request Changes]
```

### Digest mode

For long-running projects, humans can opt into daily/weekly digests instead of per-event notifications:

```
Daily Digest — April 25, 2026

3 agents active across 2 projects

Project: claw-code
  Phase 3/5 complete (60%)
  47 files changed, 1,203 insertions, 89 deletions
  134 tests passing, 0 failing
  Preview: https://claw.preview/abc123

Project: api-gateway
  Phase 1/2 complete (50%)
  12 files changed, 340 insertions, 23 deletions
  28 tests passing, 0 failing
  No action needed

[View Details] [Dashboard]
```

---

## 3. Demo Deployments

### Design goal

Humans should be able to **see and interact with** agent output without building anything locally. Each deployment is a live, auto-expiring preview tied to a specific phase or gate.

### How it works

```
Agent completes phase
  → Build Docker image from current state
  → Provision container with auto-expiry
  → Generate unique URL (or Tailscale tunnel)
  → Link deployment to phase/gate in review report
  → Human clicks link → sees live result
  → After TTL expires → container destroyed automatically
```

### Deployment types

| Type | Use case | Isolation | Access |
|------|----------|-----------|--------|
| **Local Docker** | Quick preview on developer machine | Container | localhost |
| **Tailscale tunnel** | Share local preview remotely | Container + VPN | MagicDNS URL |
| **Remote sandbox** | CI-integrated preview | Cloud container | Unique URL, auto-expiring |
| **Staging merge** | Pre-merge verification | Shared staging env | Team URL |

### Tailscale integration

For teams that want to preview agent work without exposing it publicly:

```bash
# Agent triggers:
claw deploy preview -- tailscale

# Output:
# Preview available at: https://claw-auth-feature.tail1234.ts.net
# Expires: 2026-04-26T12:00:00Z (24h)
# Phase: "Add authentication" (phase 2/5)
```

Requirements:
- Tailscale installed and authenticated
- `tailscale serve` available on PATH
- Agent container maps to a Tailscale serve endpoint

### Auto-expiry

Every preview environment has a TTL:

| Scope | Default TTL | Max TTL |
|-------|-------------|---------|
| Per-turn preview | 2 hours | 6 hours |
| Per-phase preview | 24 hours | 72 hours |
| Per-milestone preview | 7 days | 30 days |
| Manual override | Configurable | Unlimited |

Expired environments are automatically destroyed. The review report retains a static summary (screenshots, logs, test results) even after the live environment is gone.

### Phase/gate/milestone linking

Deployments are tied to the project's phase structure:

```
Project: "Build user management"
  ├── Phase 1: "Database schema"     → [Preview expired]  ✓ Approved
  ├── Phase 2: "API endpoints"       → [Live: 23h left]   ⏳ Pending review
  │   └── Preview: https://claw.preview/p2-abc123
  ├── Phase 3: "Frontend UI"         → [Not started]
  └── Phase 4: "Integration tests"   → [Not started]
```

---

## 4. Agent Orchestrator (Human Interface)

### The rip cord

Humans don't write code in this system. They **orchestrate** — setting goals, reviewing progress, and steering agents when needed.

### Orchestrator workflow

```
1. Human describes goal (natural language or structured spec)
2. Orchestrator decomposes into phases with gates
3. Each phase: agent executes → produces review artifact → human approves
4. Gate: structured checkpoint where human can redirect
5. Milestone: major deliverable with demo deployment
6. Completion: all phases done → final review → merge
```

### Human actions at any gate

| Action | Effect |
|--------|--------|
| **Approve** | Proceed to next phase |
| **Reject** | Agent retries with feedback |
| **Redirect** | Change the plan without starting over |
| **Pause** | Stop execution, preserve state |
| **Take over** | Switch from agent-driven to human-driven mode |
| **Escalate** | Request human expertise for a specific sub-problem |

### Orchestrator UI (target)

```
┌─ Project: "Build user management" ──────────────────────┐
│                                                           │
│  Phase 1: Database schema                    ✓ APPROVED   │
│  Phase 2: API endpoints                      ⏳ REVIEWING │
│  │  Agent: coder-01 running (2m 14s)                      │
│  │  Last action: wrote src/routes/users.rs                │
│  │  Tests: 5/5 passing                                   │
│  │                                                        │
│  │  [Approve Phase] [Reject] [View Live Preview]          │
│  │  [Message Agent] [Pause] [Take Over]                   │
│  │                                                        │
│  Phase 3: Frontend UI                        ○ QUEUED     │
│  Phase 4: Integration tests                   ○ QUEUED     │
│                                                           │
│  Overall: 25% │ Tokens: 23K │ Cost: $0.07 │ Time: 8m     │
└───────────────────────────────────────────────────────────┘
```

---

## 5. Accessibility & Inclusivity

### Design requirements

- **All review artifacts are screen-reader compatible** — structured HTML, not images
- **All notifications include plain-text alternatives** — no HTML-only emails
- **Color is never the only signal** — icons, labels, and patterns accompany color
- **Keyboard-first interaction** — every action accessible without a mouse
- **Configurable verbosity** — from minimal summaries to full debug output

---

## 6. Implementation Priority

| Priority | Item | Reason |
|----------|------|--------|
| P0 | Structured change reports | Humans can't review what they can't understand |
| P0 | Risk classification | Auto-approve low-risk changes to reduce human load |
| P1 | Email notifications | Lowest-effort delivery channel |
| P1 | Preview deployments (local Docker) | Verify changes without building |
| P2 | Tailscale integration | Share previews without public exposure |
| P2 | Chat integration (Slack/Discord) | Team-aware notifications |
| P2 | Orchestrator TUI | Interactive human steering |
| P3 | Mobile push | On-the-go awareness |
| P3 | Web dashboard | Always-on monitoring |
| P3 | Remote sandbox deployments | CI-integrated previews |
