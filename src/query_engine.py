from __future__ import annotations

import json
import threading
from dataclasses import dataclass, field
from uuid import uuid4

from .commands import build_command_backlog
from .models import PermissionDenial, UsageSummary
from .port_manifest import PortManifest, build_port_manifest
from .session_store import StoredSession, load_session, save_session
from .tools import build_tool_backlog
from .transcript import TranscriptStore


@dataclass(frozen=True)
class QueryEngineConfig:
    max_turns: int = 8
    max_budget_tokens: int = 2000
    compact_after_turns: int = 12
    structured_output: bool = False
    structured_retry_limit: int = 2


@dataclass(frozen=True)
class TurnResult:
    prompt: str
    output: str
    matched_commands: tuple[str, ...]
    matched_tools: tuple[str, ...]
    permission_denials: tuple[PermissionDenial, ...]
    usage: UsageSummary
    stop_reason: str
    cancel_observed: bool = False


@dataclass
class QueryEnginePort:
    manifest: PortManifest
    config: QueryEngineConfig = field(default_factory=QueryEngineConfig)
    session_id: str = field(default_factory=lambda: uuid4().hex)
    mutable_messages: list[str] = field(default_factory=list)
    permission_denials: list[PermissionDenial] = field(default_factory=list)
    total_usage: UsageSummary = field(default_factory=UsageSummary)
    transcript_store: TranscriptStore = field(default_factory=TranscriptStore)

    @classmethod
    def from_workspace(cls) -> 'QueryEnginePort':
        return cls(manifest=build_port_manifest())

    @classmethod
    def from_saved_session(cls, session_id: str) -> 'QueryEnginePort':
        stored = load_session(session_id)
        transcript = TranscriptStore(entries=list(stored.messages), flushed=True)
        return cls(
            manifest=build_port_manifest(),
            session_id=stored.session_id,
            mutable_messages=list(stored.messages),
            total_usage=UsageSummary(stored.input_tokens, stored.output_tokens),
            transcript_store=transcript,
        )

    def submit_message(
        self,
        prompt: str,
        matched_commands: tuple[str, ...] = (),
        matched_tools: tuple[str, ...] = (),
        denied_tools: tuple[PermissionDenial, ...] = (),
        cancel_event: threading.Event | None = None,
    ) -> TurnResult:
        """Submit a prompt and return a TurnResult.

        #164 Stage A: cooperative cancellation via cancel_event.

        The cancel_event argument (added for #164) lets a caller request early
        termination at a safe point. When set before the pre-mutation commit
        stage, submit_message returns early with ``stop_reason='cancelled'``
        and the engine's state (mutable_messages, transcript_store,
        permission_denials, total_usage) is left **exactly as it was on
        entry**. This closes the #161 follow-up gap: before this change, a
        wedged provider thread could finish executing and silently mutate
        state after the caller had already observed ``stop_reason='timeout'``,
        giving the session a ghost turn the caller never acknowledged.

        Contract:
          - cancel_event is None (default) — legacy behaviour, no checks.
          - cancel_event set **before** budget check — returns 'cancelled'
            immediately; no output synthesis, no projection, no mutation.
          - cancel_event set **between** budget check and commit — returns
            'cancelled' with state intact.
          - cancel_event set **after** commit — not observable; the turn is
            already committed and the caller sees 'completed'. Cancellation
            is a *safe point* mechanism, not preemption. This is the honest
            limit of cooperative cancellation in Python threading land.

        Stop reason taxonomy after #164 Stage A:
          - 'completed'            — turn committed, state mutated exactly once
          - 'max_budget_reached'   — overflow, state unchanged (#162)
          - 'max_turns_reached'    — capacity exceeded, state unchanged
          - 'cancelled'            — cancel_event observed, state unchanged
          - 'timeout'              — synthesised by runtime, not engine (#161)

        Callers that care about deadline-driven cancellation (run_turn_loop)
        can now request cleanup by setting the event on timeout — the next
        submit_message on the same engine will observe it at the start and
        return 'cancelled' without touching state, even if the previous call
        is still wedged in provider IO.
        """
        # #164 Stage A: earliest safe cancellation point. No output synthesis,
        # no budget projection, no mutation — just an immediate clean return.
        if cancel_event is not None and cancel_event.is_set():
            return TurnResult(
                prompt=prompt,
                output='',
                matched_commands=matched_commands,
                matched_tools=matched_tools,
                permission_denials=denied_tools,
                usage=self.total_usage,  # unchanged
                stop_reason='cancelled',
            )

        if len(self.mutable_messages) >= self.config.max_turns:
            output = f'Max turns reached before processing prompt: {prompt}'
            return TurnResult(
                prompt=prompt,
                output=output,
                matched_commands=matched_commands,
                matched_tools=matched_tools,
                permission_denials=denied_tools,
                usage=self.total_usage,
                stop_reason='max_turns_reached',
            )

        summary_lines = [
            f'Prompt: {prompt}',
            f'Matched commands: {", ".join(matched_commands) if matched_commands else "none"}',
            f'Matched tools: {", ".join(matched_tools) if matched_tools else "none"}',
            f'Permission denials: {len(denied_tools)}',
        ]
        output = self._format_output(summary_lines)
        projected_usage = self.total_usage.add_turn(prompt, output)

        # #162: budget check must precede mutation. Previously this block set
        # stop_reason='max_budget_reached' but still appended the overflow turn
        # to mutable_messages / transcript_store / permission_denials, corrupting
        # the session for any caller that persisted it afterwards. The overflow
        # prompt was effectively committed even though the TurnResult signalled
        # rejection. Now we early-return with pre-mutation state intact so
        # callers can safely retry with a smaller prompt or a fresh budget.
        if projected_usage.input_tokens + projected_usage.output_tokens > self.config.max_budget_tokens:
            return TurnResult(
                prompt=prompt,
                output=output,
                matched_commands=matched_commands,
                matched_tools=matched_tools,
                permission_denials=denied_tools,
                usage=self.total_usage,  # unchanged — overflow turn was rejected
                stop_reason='max_budget_reached',
            )

        # #164 Stage A: second safe cancellation point. Projection is done
        # but nothing has been committed yet. If the caller cancelled while
        # we were building output / computing budget, honour it here — still
        # no mutation.
        if cancel_event is not None and cancel_event.is_set():
            return TurnResult(
                prompt=prompt,
                output=output,
                matched_commands=matched_commands,
                matched_tools=matched_tools,
                permission_denials=denied_tools,
                usage=self.total_usage,  # unchanged
                stop_reason='cancelled',
            )

        self.mutable_messages.append(prompt)
        self.transcript_store.append(prompt)
        self.permission_denials.extend(denied_tools)
        self.total_usage = projected_usage
        self.compact_messages_if_needed()
        return TurnResult(
            prompt=prompt,
            output=output,
            matched_commands=matched_commands,
            matched_tools=matched_tools,
            permission_denials=denied_tools,
            usage=self.total_usage,
            stop_reason='completed',
        )

    def stream_submit_message(
        self,
        prompt: str,
        matched_commands: tuple[str, ...] = (),
        matched_tools: tuple[str, ...] = (),
        denied_tools: tuple[PermissionDenial, ...] = (),
    ):
        yield {'type': 'message_start', 'session_id': self.session_id, 'prompt': prompt}
        if matched_commands:
            yield {'type': 'command_match', 'commands': matched_commands}
        if matched_tools:
            yield {'type': 'tool_match', 'tools': matched_tools}
        if denied_tools:
            yield {'type': 'permission_denial', 'denials': [denial.tool_name for denial in denied_tools]}
        result = self.submit_message(prompt, matched_commands, matched_tools, denied_tools)
        yield {'type': 'message_delta', 'text': result.output}
        yield {
            'type': 'message_stop',
            'usage': {'input_tokens': result.usage.input_tokens, 'output_tokens': result.usage.output_tokens},
            'stop_reason': result.stop_reason,
            'transcript_size': len(self.transcript_store.entries),
        }

    def compact_messages_if_needed(self) -> None:
        if len(self.mutable_messages) > self.config.compact_after_turns:
            self.mutable_messages[:] = self.mutable_messages[-self.config.compact_after_turns :]
        self.transcript_store.compact(self.config.compact_after_turns)

    def replay_user_messages(self) -> tuple[str, ...]:
        return self.transcript_store.replay()

    def flush_transcript(self) -> None:
        self.transcript_store.flush()

    def persist_session(self, directory: 'Path | None' = None) -> str:
        """Flush the transcript and save the session to disk.

        Args:
            directory: Optional override for the storage directory. When None
                (default, for backward compat), uses the default location
                (``.port_sessions`` in CWD). When set, passes through to
                ``save_session`` which already supports directory overrides.

        #166: added directory parameter to match the session-lifecycle CLI
        surface established by #160/#165. Claws running out-of-tree can now
        redirect session creation to a workspace-specific dir without chdir.
        """
        self.flush_transcript()
        path = save_session(
            StoredSession(
                session_id=self.session_id,
                messages=tuple(self.mutable_messages),
                input_tokens=self.total_usage.input_tokens,
                output_tokens=self.total_usage.output_tokens,
            ),
            directory,
        )
        return str(path)

    def _format_output(self, summary_lines: list[str]) -> str:
        if self.config.structured_output:
            payload = {
                'summary': summary_lines,
                'session_id': self.session_id,
            }
            return self._render_structured_output(payload)
        return '\n'.join(summary_lines)

    def _render_structured_output(self, payload: dict[str, object]) -> str:
        last_error: Exception | None = None
        for _ in range(self.config.structured_retry_limit):
            try:
                return json.dumps(payload, indent=2)
            except (TypeError, ValueError) as exc:  # pragma: no cover - defensive branch
                last_error = exc
                payload = {'summary': ['structured output retry'], 'session_id': self.session_id}
        raise RuntimeError('structured output rendering failed') from last_error

    def render_summary(self) -> str:
        command_backlog = build_command_backlog()
        tool_backlog = build_tool_backlog()
        sections = [
            '# Python Porting Workspace Summary',
            '',
            self.manifest.to_markdown(),
            '',
            f'Command surface: {len(command_backlog.modules)} mirrored entries',
            *command_backlog.summary_lines()[:10],
            '',
            f'Tool surface: {len(tool_backlog.modules)} mirrored entries',
            *tool_backlog.summary_lines()[:10],
            '',
            f'Session id: {self.session_id}',
            f'Conversation turns stored: {len(self.mutable_messages)}',
            f'Permission denials tracked: {len(self.permission_denials)}',
            f'Usage totals: in={self.total_usage.input_tokens} out={self.total_usage.output_tokens}',
            f'Max turns: {self.config.max_turns}',
            f'Max budget tokens: {self.config.max_budget_tokens}',
            f'Transcript flushed: {self.transcript_store.flushed}',
        ]
        return '\n'.join(sections)
