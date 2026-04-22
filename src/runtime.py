from __future__ import annotations

import time
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FuturesTimeoutError
from dataclasses import dataclass

from .commands import PORTED_COMMANDS
from .context import PortContext, build_port_context, render_context
from .history import HistoryLog
from .models import PermissionDenial, PortingModule, UsageSummary
from .query_engine import QueryEngineConfig, QueryEnginePort, TurnResult
from .setup import SetupReport, WorkspaceSetup, run_setup
from .system_init import build_system_init_message
from .tools import PORTED_TOOLS
from .execution_registry import build_execution_registry


@dataclass(frozen=True)
class RoutedMatch:
    kind: str
    name: str
    source_hint: str
    score: int


@dataclass
class RuntimeSession:
    prompt: str
    context: PortContext
    setup: WorkspaceSetup
    setup_report: SetupReport
    system_init_message: str
    history: HistoryLog
    routed_matches: list[RoutedMatch]
    turn_result: TurnResult
    command_execution_messages: tuple[str, ...]
    tool_execution_messages: tuple[str, ...]
    stream_events: tuple[dict[str, object], ...]
    persisted_session_path: str

    def as_markdown(self) -> str:
        lines = [
            '# Runtime Session',
            '',
            f'Prompt: {self.prompt}',
            '',
            '## Context',
            render_context(self.context),
            '',
            '## Setup',
            f'- Python: {self.setup.python_version} ({self.setup.implementation})',
            f'- Platform: {self.setup.platform_name}',
            f'- Test command: {self.setup.test_command}',
            '',
            '## Startup Steps',
            *(f'- {step}' for step in self.setup.startup_steps()),
            '',
            '## System Init',
            self.system_init_message,
            '',
            '## Routed Matches',
        ]
        if self.routed_matches:
            lines.extend(
                f'- [{match.kind}] {match.name} ({match.score}) — {match.source_hint}'
                for match in self.routed_matches
            )
        else:
            lines.append('- none')
        lines.extend([
            '',
            '## Command Execution',
            *(self.command_execution_messages or ('none',)),
            '',
            '## Tool Execution',
            *(self.tool_execution_messages or ('none',)),
            '',
            '## Stream Events',
            *(f"- {event['type']}: {event}" for event in self.stream_events),
            '',
            '## Turn Result',
            self.turn_result.output,
            '',
            f'Persisted session path: {self.persisted_session_path}',
            '',
            self.history.as_markdown(),
        ])
        return '\n'.join(lines)


class PortRuntime:
    def route_prompt(self, prompt: str, limit: int = 5) -> list[RoutedMatch]:
        tokens = {token.lower() for token in prompt.replace('/', ' ').replace('-', ' ').split() if token}
        by_kind = {
            'command': self._collect_matches(tokens, PORTED_COMMANDS, 'command'),
            'tool': self._collect_matches(tokens, PORTED_TOOLS, 'tool'),
        }

        selected: list[RoutedMatch] = []
        for kind in ('command', 'tool'):
            if by_kind[kind]:
                selected.append(by_kind[kind].pop(0))

        leftovers = sorted(
            [match for matches in by_kind.values() for match in matches],
            key=lambda item: (-item.score, item.kind, item.name),
        )
        selected.extend(leftovers[: max(0, limit - len(selected))])
        return selected[:limit]

    def bootstrap_session(self, prompt: str, limit: int = 5) -> RuntimeSession:
        context = build_port_context()
        setup_report = run_setup(trusted=True)
        setup = setup_report.setup
        history = HistoryLog()
        engine = QueryEnginePort.from_workspace()
        history.add('context', f'python_files={context.python_file_count}, archive_available={context.archive_available}')
        history.add('registry', f'commands={len(PORTED_COMMANDS)}, tools={len(PORTED_TOOLS)}')
        matches = self.route_prompt(prompt, limit=limit)
        registry = build_execution_registry()
        command_execs = tuple(registry.command(match.name).execute(prompt) for match in matches if match.kind == 'command' and registry.command(match.name))
        tool_execs = tuple(registry.tool(match.name).execute(prompt) for match in matches if match.kind == 'tool' and registry.tool(match.name))
        denials = tuple(self._infer_permission_denials(matches))
        stream_events = tuple(engine.stream_submit_message(
            prompt,
            matched_commands=tuple(match.name for match in matches if match.kind == 'command'),
            matched_tools=tuple(match.name for match in matches if match.kind == 'tool'),
            denied_tools=denials,
        ))
        turn_result = engine.submit_message(
            prompt,
            matched_commands=tuple(match.name for match in matches if match.kind == 'command'),
            matched_tools=tuple(match.name for match in matches if match.kind == 'tool'),
            denied_tools=denials,
        )
        persisted_session_path = engine.persist_session()
        history.add('routing', f'matches={len(matches)} for prompt={prompt!r}')
        history.add('execution', f'command_execs={len(command_execs)} tool_execs={len(tool_execs)}')
        history.add('turn', f'commands={len(turn_result.matched_commands)} tools={len(turn_result.matched_tools)} denials={len(turn_result.permission_denials)} stop={turn_result.stop_reason}')
        history.add('session_store', persisted_session_path)
        return RuntimeSession(
            prompt=prompt,
            context=context,
            setup=setup,
            setup_report=setup_report,
            system_init_message=build_system_init_message(trusted=True),
            history=history,
            routed_matches=matches,
            turn_result=turn_result,
            command_execution_messages=command_execs,
            tool_execution_messages=tool_execs,
            stream_events=stream_events,
            persisted_session_path=persisted_session_path,
        )

    def run_turn_loop(
        self,
        prompt: str,
        limit: int = 5,
        max_turns: int = 3,
        structured_output: bool = False,
        timeout_seconds: float | None = None,
        continuation_prompt: str | None = None,
    ) -> list[TurnResult]:
        """Run a multi-turn engine loop with optional wall-clock deadline.

        Args:
            prompt: The initial prompt to submit.
            limit: Match routing limit.
            max_turns: Maximum number of turns before stopping.
            structured_output: Whether to request structured output.
            timeout_seconds: Total wall-clock budget across all turns. When the
                budget is exhausted mid-turn, a synthetic TurnResult with
                ``stop_reason='timeout'`` is appended and the loop exits.
                ``None`` (default) preserves legacy unbounded behaviour.
            continuation_prompt: What to send on turns after the first. When
                ``None`` (default, #163), the loop stops after turn 0 and the
                caller decides how to continue. When set, the same text is
                submitted for every turn after the first, giving claws a clean
                hook for structured follow-ups (e.g. ``"Continue."``, a
                routing-planner instruction, or a tool-output cue). Previously
                the loop silently appended ``" [turn N]"`` to the original
                prompt, polluting the transcript with harness-generated
                annotation the model had no way to interpret.

        Returns:
            A list of TurnResult objects. The final entry's ``stop_reason``
            distinguishes ``'completed'``, ``'max_turns_reached'``,
            ``'max_budget_reached'``, or ``'timeout'``.

        #161: prior to this change a hung ``engine.submit_message`` call would
        block the loop indefinitely with no cancellation path, forcing claws to
        rely on external watchdogs or OS-level kills. Callers can now enforce a
        deadline and receive a typed timeout signal instead.

        #163: the old ``f'{prompt} [turn {turn + 1}]'`` suffix was never
        interpreted by the engine or any system prompt. It looked like a real
        user turn in ``mutable_messages`` and the transcript, making replay and
        analysis fragile. Removed entirely; callers supply ``continuation_prompt``
        for meaningful follow-ups or let the loop stop after turn 0.
        """
        engine = QueryEnginePort.from_workspace()
        engine.config = QueryEngineConfig(max_turns=max_turns, structured_output=structured_output)
        matches = self.route_prompt(prompt, limit=limit)
        command_names = tuple(match.name for match in matches if match.kind == 'command')
        tool_names = tuple(match.name for match in matches if match.kind == 'tool')
        # #159: infer permission denials from the routed matches, not hardcoded empty tuple.
        # Multi-turn sessions must have the same security posture as bootstrap_session.
        denied_tools = tuple(self._infer_permission_denials(matches))
        results: list[TurnResult] = []
        deadline = time.monotonic() + timeout_seconds if timeout_seconds is not None else None

        # ThreadPoolExecutor is reused across turns so we cancel cleanly on exit.
        executor = ThreadPoolExecutor(max_workers=1) if deadline is not None else None
        try:
            for turn in range(max_turns):
                # #163: no more f'{prompt} [turn N]' suffix injection.
                # On turn 0 submit the original prompt.
                # On turn > 0, submit the caller-supplied continuation prompt;
                # if the caller did not supply one, stop the loop cleanly instead
                # of fabricating a fake user turn.
                if turn == 0:
                    turn_prompt = prompt
                elif continuation_prompt is not None:
                    turn_prompt = continuation_prompt
                else:
                    break

                if deadline is None:
                    # Legacy path: unbounded call, preserves existing behaviour exactly.
                    # #159: pass inferred denied_tools (no longer hardcoded empty tuple)
                    result = engine.submit_message(turn_prompt, command_names, tool_names, denied_tools)
                else:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        results.append(self._build_timeout_result(turn_prompt, command_names, tool_names))
                        break
                    assert executor is not None
                    future = executor.submit(
                        engine.submit_message, turn_prompt, command_names, tool_names, denied_tools
                    )
                    try:
                        result = future.result(timeout=remaining)
                    except FuturesTimeoutError:
                        # Best-effort cancel; submit_message may still finish in background
                        # but we never read its output. The engine's own state mutation
                        # is owned by the engine and not our concern here.
                        future.cancel()
                        results.append(self._build_timeout_result(turn_prompt, command_names, tool_names))
                        break

                results.append(result)
                if result.stop_reason != 'completed':
                    break
        finally:
            if executor is not None:
                # wait=False: don't let a hung thread block loop exit indefinitely.
                # The thread will be reaped when the interpreter shuts down or when
                # the engine call eventually returns.
                executor.shutdown(wait=False)
        return results

    @staticmethod
    def _build_timeout_result(
        prompt: str,
        command_names: tuple[str, ...],
        tool_names: tuple[str, ...],
    ) -> TurnResult:
        """Synthesize a TurnResult representing a wall-clock timeout (#161)."""
        return TurnResult(
            prompt=prompt,
            output='Wall-clock timeout exceeded before turn completed.',
            matched_commands=command_names,
            matched_tools=tool_names,
            permission_denials=(),
            usage=UsageSummary(),
            stop_reason='timeout',
        )

    def _infer_permission_denials(self, matches: list[RoutedMatch]) -> list[PermissionDenial]:
        denials: list[PermissionDenial] = []
        for match in matches:
            if match.kind == 'tool' and 'bash' in match.name.lower():
                denials.append(PermissionDenial(tool_name=match.name, reason='destructive shell execution remains gated in the Python port'))
        return denials

    def _collect_matches(self, tokens: set[str], modules: tuple[PortingModule, ...], kind: str) -> list[RoutedMatch]:
        matches: list[RoutedMatch] = []
        for module in modules:
            score = self._score(tokens, module)
            if score > 0:
                matches.append(RoutedMatch(kind=kind, name=module.name, source_hint=module.source_hint, score=score))
        matches.sort(key=lambda item: (-item.score, item.name))
        return matches

    @staticmethod
    def _score(tokens: set[str], module: PortingModule) -> int:
        haystacks = [module.name.lower(), module.source_hint.lower(), module.responsibility.lower()]
        score = 0
        for token in tokens:
            if any(token in haystack for haystack in haystacks):
                score += 1
        return score
