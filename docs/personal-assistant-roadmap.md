# From Claw Code to a Personal AI Assistant (Life OS)

This document turns the current “developer CLI agent” direction into a concrete path toward a **personal AI assistant**: a multi-channel interface (chat/voice), personal memory (RAG for life), tool/action integrations (MCP + plugins), proactivity (OmX-style loops), and long-lived identity (sessions + profile).

It is intentionally pragmatic: each section has **MVP scope**, **next step**, and **evolution**.

---

## 1) Interface: out of the terminal

### Goal
Make `claw` usable without opening an IDE or terminal — from a phone, from chat, and eventually by voice.

### MVP
- **Chat bridge**: a small service that relays messages from **Discord** (primary) or Telegram to `claw` / `claw-analog`.
  - Treat the chat thread as the “front-end”, and `claw` as the execution runtime.
  - Map a channel/thread to a **session id** (resume/append).
- **Basic UX**: slash-like commands in chat:
  - `/prompt …`, `/resume latest`, `/status`, `/cost`, `/help`
  - “safe mode” defaults (read-only) unless elevated explicitly.

### Next step
- **Voice**:
  - Speech-to-text input (e.g. Whisper-class STT) into the same chat bridge.
  - Text-to-speech output for hands-free feedback.

### Evolution
- Multi-modal: attachments (images/PDF) routed into ingest/personal memory.
- Presence and notifications: summaries pushed back into chat.

---

## 2) Memory: from “RAG for code” to “RAG for life”

### Goal
Let the assistant answer personal questions and make decisions using *your* long-term context, not only the current repo.

### MVP
- Extend ingestion inputs beyond git workspaces:
  - Notes (Markdown), exported chats, simple text logs.
  - PDFs (initially text extraction outside Rust is OK; later: built-in pipeline).
- Keep a clear separation:
  - **Work RAG** (code/workspaces)
  - **Personal RAG** (notes, plans, history)

### Next step
- Evolve `retrieve_context` into a **multi-source retrieval tool**:
  - “where to search” selector (work/personal/both)
  - metadata filters (source, date ranges, tags)

### Evolution
- Incremental ingestion + event-based updates (watch folders, chat events).
- Better stores (ANN/Qdrant/etc) when scale demands it.

---

## 3) Hands: tools, MCP, plugins

### Goal
The assistant is valuable because it can **do** things, not only talk.

### MVP
- Wire in external systems via **MCP servers**:
  - Calendar, notes (Notion), email, task trackers, smart home (as available).
- Establish a convention for “personal skills”:
  - a dedicated directory (e.g. `.claw/skills/`) for user-specific automations
  - small, composable tools (digest, budgeting, reminders) rather than monoliths

### Next step
- “Tool discovery” UX: list available MCP/tools/skills directly from chat.
- Permission boundaries per tool category (read vs write, destructive actions require explicit confirmation).

### Evolution
- Plugin marketplace flows for reusing “skills”.
- Audit logging and replay of actions.

---

## 4) Proactivity: OmX-style loops

### Goal
Move from reactive “answer me” to proactive “notice + prepare + propose + execute”.

### MVP
- A scheduled runner that periodically:
  - checks inbox/notifications
  - extracts actionable tasks
  - drafts responses
  - posts a short digest to chat

### Next step
- Multi-agent patterns (Architect/Executor/Reviewer) for higher reliability:
  - executor proposes actions
  - reviewer validates safety and correctness
  - only then does the bridge run the write/action tool

### Evolution
- Event-driven triggers (webhooks) instead of only cron.
- “Autopilot” modes with bounded scopes (time, tools, spend limits).

---

## 5) Long-lived identity: sessions + profile

### Goal
Make the assistant feel continuous and personalized across days/weeks.

### MVP
- Default to resuming the latest session (`--resume latest`-style behavior).
- Use a short, user-owned profile/system-prompt for tone and preferences.

### Next step
- Separate:
  - “personality” (style, preferences)
  - “memory” (facts, history)
  - “policies” (permissions, safety rules)

### Evolution
- Multiple personas (work/personal) with explicit switching.
- Transparent memory controls (“forget this”, “store this”).

---

## Suggested milestone sequence

1. **Discord bridge + session mapping** (no new AI capabilities; just distribution).
2. **Personal ingest source #1** (notes folder) + retrieval selector (personal/work).
3. **One MCP integration** (calendar or notes) + a single “daily digest” skill.
4. **Scheduled digest loop** (cron) with bounded permissions.
5. **Voice input/output** on top of the same bridge.

