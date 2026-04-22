"""Tests for --output-format on command-graph and tool-pool (ROADMAP #169).

Diagnostic inventory surfaces now speak the CLI family's JSON contract.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))


def _run(args: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, '-m', 'src.main', *args],
        cwd=Path(__file__).resolve().parent.parent,
        capture_output=True,
        text=True,
    )


class TestCommandGraphOutputFormat:
    def test_command_graph_json(self) -> None:
        result = _run(['command-graph', '--output-format', 'json'])
        assert result.returncode == 0, result.stderr

        envelope = json.loads(result.stdout)
        assert 'builtins_count' in envelope
        assert 'plugin_like_count' in envelope
        assert 'skill_like_count' in envelope
        assert 'total_count' in envelope
        assert envelope['total_count'] == (
            envelope['builtins_count'] + envelope['plugin_like_count'] + envelope['skill_like_count']
        )
        assert isinstance(envelope['builtins'], list)
        if envelope['builtins']:
            assert set(envelope['builtins'][0].keys()) == {'name', 'source_hint'}

    def test_command_graph_text_backward_compat(self) -> None:
        result = _run(['command-graph'])
        assert result.returncode == 0
        assert '# Command Graph' in result.stdout
        assert 'Builtins:' in result.stdout
        # Not JSON
        assert not result.stdout.strip().startswith('{')


class TestToolPoolOutputFormat:
    def test_tool_pool_json(self) -> None:
        result = _run(['tool-pool', '--output-format', 'json'])
        assert result.returncode == 0, result.stderr

        envelope = json.loads(result.stdout)
        assert 'simple_mode' in envelope
        assert 'include_mcp' in envelope
        assert 'tool_count' in envelope
        assert 'tools' in envelope
        assert envelope['tool_count'] == len(envelope['tools'])
        if envelope['tools']:
            assert set(envelope['tools'][0].keys()) == {'name', 'source_hint'}

    def test_tool_pool_text_backward_compat(self) -> None:
        result = _run(['tool-pool'])
        assert result.returncode == 0
        assert '# Tool Pool' in result.stdout
        assert 'Simple mode:' in result.stdout
        assert not result.stdout.strip().startswith('{')
