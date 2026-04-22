"""Cross-surface CLI parity audit (ROADMAP #171).

Prevents future drift of the unified JSON envelope contract across
claw-code's CLI surface. Instead of requiring humans to notice when
a new command skips --output-format, this test introspects the parser
at runtime and verifies every command in the declared clawable-surface
list supports --output-format {text,json}.

When a new clawable-surface command is added:
  1. Implement --output-format on the subparser (normal feature work).
  2. Add the command name to CLAWABLE_SURFACES below.
  3. This test passes automatically.

When a developer adds a new clawable-surface command but forgets
--output-format, the test fails with a concrete message pointing at
the missing flag. Claws no longer need to eyeball parity; the contract
is enforced at test time.

Three classes of commands:
  - CLAWABLE_SURFACES: MUST accept --output-format (inspect/lifecycle/exec/diagnostic)
  - OPT_OUT_SURFACES: explicitly exempt (simulation/mode commands, human-first diagnostic)
  - Any command in parser not listed in either: test FAILS with classification request

This is operationalised parity — a machine-first CLI enforced by a
machine-first test.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from src.main import build_parser  # noqa: E402


# Commands that MUST accept --output-format {text,json}.
# These are the machine-first surfaces — session lifecycle, execution,
# inspect, diagnostic inventory.
CLAWABLE_SURFACES = frozenset({
    # Session lifecycle (#160, #165, #166)
    'list-sessions',
    'delete-session',
    'load-session',
    'flush-transcript',
    # Inspect (#167)
    'show-command',
    'show-tool',
    # Execution/work-verb (#168)
    'exec-command',
    'exec-tool',
    'route',
    'bootstrap',
    # Diagnostic inventory (#169, #170)
    'command-graph',
    'tool-pool',
    'bootstrap-graph',
    # Turn-loop with JSON output (#164 Stage B, #174)
    'turn-loop',
})

# Commands explicitly exempt from --output-format requirement.
# Rationale must be explicit — either the command is human-first
# (rich Markdown docs/reports), simulation-only, or has a dedicated
# JSON mode flag under a different name.
OPT_OUT_SURFACES = frozenset({
    # Rich-Markdown report commands (planned future: JSON schema)
    'summary',            # full workspace summary (Markdown)
    'manifest',           # workspace manifest (Markdown)
    'parity-audit',       # TypeScript archive comparison (Markdown)
    'setup-report',       # startup/prefetch report (Markdown)
    # List commands with their own query/filter surface (not JSON yet)
    'subsystems',         # use --limit
    'commands',           # use --query / --limit / --no-plugin-commands
    'tools',              # use --query / --limit / --simple-mode
    # Simulation/debug surfaces (not claw-orchestrated)
    'remote-mode',
    'ssh-mode',
    'teleport-mode',
    'direct-connect-mode',
    'deep-link-mode',
})


def _discover_subcommands_and_flags() -> dict[str, frozenset[str]]:
    """Introspect the argparse tree to discover every subcommand and its flags.

    Returns:
      {subcommand_name: frozenset of option strings including --output-format
       if registered}
    """
    parser = build_parser()
    subcommand_flags: dict[str, frozenset[str]] = {}
    for action in parser._actions:
        if not hasattr(action, 'choices') or not action.choices:
            continue
        if action.dest != 'command':
            continue
        for name, subp in action.choices.items():
            flags: set[str] = set()
            for a in subp._actions:
                if a.option_strings:
                    flags.update(a.option_strings)
            subcommand_flags[name] = frozenset(flags)
    return subcommand_flags


class TestClawableSurfaceParity:
    """Every clawable-surface command MUST accept --output-format {text,json}.

    This is the invariant that codifies 'claws can treat the CLI as a
    unified protocol without special-casing'.
    """

    def test_all_clawable_surfaces_accept_output_format(self) -> None:
        """All commands in CLAWABLE_SURFACES must have --output-format registered."""
        subcommand_flags = _discover_subcommands_and_flags()
        missing = []
        for cmd in CLAWABLE_SURFACES:
            if cmd not in subcommand_flags:
                missing.append(f'{cmd}: not registered in parser')
            elif '--output-format' not in subcommand_flags[cmd]:
                missing.append(f'{cmd}: missing --output-format flag')
        assert not missing, (
            'Clawable-surface parity violation. Every command in '
            'CLAWABLE_SURFACES must accept --output-format. Failures:\n'
            + '\n'.join(f'  - {m}' for m in missing)
        )

    @pytest.mark.parametrize('cmd_name', sorted(CLAWABLE_SURFACES))
    def test_clawable_surface_output_format_choices(self, cmd_name: str) -> None:
        """Every clawable surface must accept exactly {text, json} choices."""
        parser = build_parser()
        for action in parser._actions:
            if not hasattr(action, 'choices') or not action.choices:
                continue
            if action.dest != 'command':
                continue
            if cmd_name not in action.choices:
                continue
            subp = action.choices[cmd_name]
            for a in subp._actions:
                if '--output-format' in a.option_strings:
                    assert a.choices == ['text', 'json'], (
                        f'{cmd_name}: --output-format choices are {a.choices}, '
                        f'expected [text, json]'
                    )
                    assert a.default == 'text', (
                        f'{cmd_name}: --output-format default is {a.default!r}, '
                        f'expected \'text\' for backward compat'
                    )
                    return
        pytest.fail(f'{cmd_name}: no --output-format flag found')


class TestCommandClassificationCoverage:
    """Every registered subcommand must be classified as either CLAWABLE or OPT_OUT.

    If a new command is added to the parser but forgotten in both sets, this
    test fails loudly — forcing an explicit classification decision.
    """

    def test_every_registered_command_is_classified(self) -> None:
        subcommand_flags = _discover_subcommands_and_flags()
        all_classified = CLAWABLE_SURFACES | OPT_OUT_SURFACES
        unclassified = set(subcommand_flags.keys()) - all_classified
        assert not unclassified, (
            'Unclassified subcommands detected. Every new command must be '
            'explicitly added to either CLAWABLE_SURFACES (must accept '
            '--output-format) or OPT_OUT_SURFACES (explicitly exempt with '
            'rationale). Unclassified:\n'
            + '\n'.join(f'  - {cmd}' for cmd in sorted(unclassified))
        )

    def test_no_command_in_both_sets(self) -> None:
        """Sanity: a command cannot be both clawable AND opt-out."""
        overlap = CLAWABLE_SURFACES & OPT_OUT_SURFACES
        assert not overlap, (
            f'Classification conflict: commands appear in both sets: {overlap}'
        )

    def test_all_classified_commands_actually_exist(self) -> None:
        """No typos — every command in our sets must actually be registered."""
        subcommand_flags = _discover_subcommands_and_flags()
        ghosts = (CLAWABLE_SURFACES | OPT_OUT_SURFACES) - set(subcommand_flags.keys())
        assert not ghosts, (
            f'Phantom commands in classification sets (not in parser): {ghosts}. '
            'Update CLAWABLE_SURFACES / OPT_OUT_SURFACES if commands were removed.'
        )


class TestJsonOutputContractEndToEnd:
    """Verify the contract AT RUNTIME — not just parser-level, but actual execution.

    Each clawable command must, when invoked with --output-format json,
    produce parseable JSON on stdout (for success cases).
    """

    # Minimal invocation args for each clawable command (to hit success path)
    RUNTIME_INVOCATIONS = {
        'list-sessions': [],
        # delete-session/load-session: skip (need state setup, covered by dedicated tests)
        'show-command': ['add-dir'],
        'show-tool': ['BashTool'],
        'exec-command': ['add-dir', 'hi'],
        'exec-tool': ['BashTool', '{}'],
        'route': ['review'],
        'bootstrap': ['hello'],
        'command-graph': [],
        'tool-pool': [],
        'bootstrap-graph': [],
        # flush-transcript: skip (creates files, covered by dedicated tests)
    }

    @pytest.mark.parametrize('cmd_name,cmd_args', sorted(RUNTIME_INVOCATIONS.items()))
    def test_command_emits_parseable_json(self, cmd_name: str, cmd_args: list[str]) -> None:
        """End-to-end: invoking with --output-format json yields valid JSON."""
        import json
        result = subprocess.run(
            [sys.executable, '-m', 'src.main', cmd_name, *cmd_args, '--output-format', 'json'],
            cwd=Path(__file__).resolve().parent.parent,
            capture_output=True,
            text=True,
        )
        # Accept exit 0 (success) or 1 (typed not-found) — both must still produce JSON
        assert result.returncode in (0, 1), (
            f'{cmd_name}: unexpected exit {result.returncode}\n'
            f'stderr: {result.stderr}\n'
            f'stdout: {result.stdout[:200]}'
        )
        try:
            json.loads(result.stdout)
        except json.JSONDecodeError as e:
            pytest.fail(
                f'{cmd_name} {cmd_args} --output-format json did not produce '
                f'parseable JSON: {e}\nOutput: {result.stdout[:200]}'
            )


class TestOptOutSurfaceRejection:
    """Cycle #30: OPT_OUT surfaces must REJECT --output-format, not silently accept.
    
    OPT_OUT_AUDIT.md classifies 12 surfaces as intentionally exempt from the
    JSON envelope contract. This test LOCKS that rejection so accidental
    drift (e.g., a developer adds --output-format to summary without thinking)
    doesn't silently promote an OPT_OUT surface to CLAWABLE.
    
    Relationship to existing tests:
    - test_clawable_surface_has_output_format: asserts CLAWABLE surfaces accept it
    - TestOptOutSurfaceRejection: asserts OPT_OUT surfaces REJECT it
    
    Together, these two test classes form a complete parity check:
    every surface is either IN or OUT, and both cases are explicitly tested.
    
    If an OPT_OUT surface is promoted to CLAWABLE intentionally:
    1. Move it from OPT_OUT_SURFACES to CLAWABLE_SURFACES
    2. Update OPT_OUT_AUDIT.md with promotion rationale
    3. Remove from this test's expected rejections
    4. Both sets of tests continue passing
    """

    @pytest.mark.parametrize('cmd_name', sorted(OPT_OUT_SURFACES))
    def test_opt_out_surface_rejects_output_format(self, cmd_name: str) -> None:
        """OPT_OUT surfaces must NOT accept --output-format flag.
        
        Passing --output-format to an OPT_OUT surface should produce an
        'unrecognized arguments' error from argparse.
        """
        result = subprocess.run(
            [sys.executable, '-m', 'src.main', cmd_name, '--output-format', 'json'],
            cwd=Path(__file__).resolve().parent.parent,
            capture_output=True,
            text=True,
        )
        # Should fail — argparse exit 2 in text mode, exit 1 in JSON mode
        # (both modes normalize to "unrecognized arguments" message)
        assert result.returncode != 0, (
            f'{cmd_name} unexpectedly accepted --output-format json. '
            f'If this is intentional (promotion to CLAWABLE), move from '
            f'OPT_OUT_SURFACES to CLAWABLE_SURFACES and update OPT_OUT_AUDIT.md. '
            f'Output: {result.stdout[:200]}\nStderr: {result.stderr[:200]}'
        )
        # Verify the error is specifically about --output-format
        error_text = result.stdout + result.stderr
        assert '--output-format' in error_text or 'unrecognized' in error_text, (
            f'{cmd_name} failed but error not about --output-format. '
            f'Something else is broken:\n'
            f'stdout: {result.stdout[:300]}\nstderr: {result.stderr[:300]}'
        )

    def test_opt_out_set_matches_audit_document(self) -> None:
        """OPT_OUT_SURFACES constant must exactly match OPT_OUT_AUDIT.md listing.
        
        This test reads OPT_OUT_AUDIT.md and verifies the constant doesn't
        drift from the documentation.
        """
        audit_path = Path(__file__).resolve().parent.parent / 'OPT_OUT_AUDIT.md'
        audit_text = audit_path.read_text()
        
        # Expected 12 surfaces per audit doc
        expected_surfaces = {
            # Group A: Rich-Markdown Reports (4)
            'summary', 'manifest', 'parity-audit', 'setup-report',
            # Group B: List Commands (3)
            'subsystems', 'commands', 'tools',
            # Group C: Simulation/Debug (5)
            'remote-mode', 'ssh-mode', 'teleport-mode',
            'direct-connect-mode', 'deep-link-mode',
        }
        
        assert OPT_OUT_SURFACES == expected_surfaces, (
            f'OPT_OUT_SURFACES drift from expected 12 surfaces per audit:\n'
            f'  Expected: {sorted(expected_surfaces)}\n'
            f'  Actual:   {sorted(OPT_OUT_SURFACES)}'
        )
        
        # Each surface should be mentioned in audit doc
        missing_from_audit = [s for s in OPT_OUT_SURFACES if s not in audit_text]
        assert not missing_from_audit, (
            f'OPT_OUT surfaces not mentioned in OPT_OUT_AUDIT.md: {missing_from_audit}'
        )

    def test_opt_out_count_matches_declared(self) -> None:
        """OPT_OUT_AUDIT.md declares '12 surfaces'. Constant must match."""
        assert len(OPT_OUT_SURFACES) == 12, (
            f'OPT_OUT_SURFACES has {len(OPT_OUT_SURFACES)} items, '
            f'but OPT_OUT_AUDIT.md declares 12 total surfaces. '
            f'Update either the audit doc or the constant.'
        )
