"""Tests for run_turn_loop permission denials parity (ROADMAP #159).

Verifies that multi-turn sessions have the same security posture as
single-turn bootstrap_session: denied_tools are inferred from matches
and threaded through every turn, not hardcoded empty.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from src.runtime import PortRuntime  # noqa: E402


class TestPermissionDenialsInTurnLoop:
    """#159: permission denials must be non-empty in run_turn_loop,
    matching what bootstrap_session produces for the same prompt.
    """

    def test_turn_loop_surfaces_permission_denials_like_bootstrap(self) -> None:
        """Symmetry check: turn_loop and bootstrap_session infer the same denials."""
        runtime = PortRuntime()
        prompt = 'run bash ls'

        # Single-turn via bootstrap
        bootstrap_result = runtime.bootstrap_session(prompt)
        bootstrap_denials = bootstrap_result.turn_result.permission_denials

        # Multi-turn via run_turn_loop (single turn, no continuation)
        loop_results = runtime.run_turn_loop(prompt, max_turns=1)
        loop_denials = loop_results[0].permission_denials

        # Both should infer denials for bash-family tools
        assert len(bootstrap_denials) > 0, (
            'bootstrap_session should deny bash-family tools'
        )
        assert len(loop_denials) > 0, (
            f'#159 regression: run_turn_loop returned empty denials; '
            f'expected {len(bootstrap_denials)} like bootstrap_session'
        )

        # The denial kinds should match (both deny the same tools)
        bootstrap_denied_names = {d.tool_name for d in bootstrap_denials}
        loop_denied_names = {d.tool_name for d in loop_denials}
        assert bootstrap_denied_names == loop_denied_names, (
            f'asymmetric denials: bootstrap denied {bootstrap_denied_names}, '
            f'loop denied {loop_denied_names}'
        )

    def test_turn_loop_with_continuation_preserves_denials(self) -> None:
        """Denials are inferred once at loop start, then passed to every turn."""
        runtime = PortRuntime()
        from unittest.mock import patch

        with patch('src.runtime.QueryEnginePort.from_workspace') as mock_factory:
            from src.models import UsageSummary
            from src.query_engine import TurnResult

            engine = mock_factory.return_value
            submitted_denials: list[tuple] = []

            def _capture(prompt, commands, tools, denials):
                submitted_denials.append(denials)
                return TurnResult(
                    prompt=prompt,
                    output='ok',
                    matched_commands=(),
                    matched_tools=(),
                    permission_denials=denials,  # echo back the denials
                    usage=UsageSummary(),
                    stop_reason='completed',
                )

            engine.submit_message.side_effect = _capture

            loop_results = runtime.run_turn_loop(
                'run bash rm', max_turns=2, continuation_prompt='continue'
            )

            # Both turn 0 and turn 1 should have received the same denials
            assert len(submitted_denials) == 2
            assert submitted_denials[0] == submitted_denials[1], (
                'denials should be consistent across all turns'
            )
            # And they should be non-empty (bash is destructive)
            assert len(submitted_denials[0]) > 0, (
                'turn-loop denials were empty — #159 regression'
            )

            # Turn results should reflect the denials that were passed
            for result in loop_results:
                assert len(result.permission_denials) > 0
