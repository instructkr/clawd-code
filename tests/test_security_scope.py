from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

from src.models import PermissionDenial
from src.path_scope import WorkspacePathScope, extract_path_candidates
from src.permissions import ToolPermissionContext
from src.query_engine import QueryEnginePort
from src.tools import execute_tool


class WorkspacePathScopeTests(unittest.TestCase):
    def test_direct_parent_escape_is_denied(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / 'workspace'
            workspace.mkdir()
            decision = WorkspacePathScope.from_root(workspace).validate_payload('cat ../secret.txt')
            self.assertFalse(decision.allowed)
            self.assertIn('outside workspace scope', decision.reason)

    def test_issue_3007_symlink_escape_is_denied(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workspace = root / 'workspace'
            outside = root / 'outside'
            workspace.mkdir()
            outside.mkdir()
            (outside / 'secret.txt').write_text('secret')
            link = workspace / 'linked-outside'
            link.symlink_to(outside, target_is_directory=True)

            decision = WorkspacePathScope.from_root(workspace).validate_payload('cat linked-outside/secret.txt')

            self.assertFalse(decision.allowed)
            self.assertIn(str(outside.resolve()), decision.resolved or '')

    def test_glob_expansion_must_stay_inside_workspace(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workspace = root / 'workspace'
            outside = root / 'outside'
            workspace.mkdir()
            outside.mkdir()
            (outside / 'secret.txt').write_text('secret')

            decision = WorkspacePathScope.from_root(workspace).validate_payload(f'cat {outside}/*.txt')

            self.assertFalse(decision.allowed)
            self.assertEqual(str((outside / 'secret.txt').resolve()), decision.resolved)

    def test_shell_environment_expansion_is_validated(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workspace = root / 'workspace'
            outside = root / 'outside'
            workspace.mkdir()
            outside.mkdir()
            previous = os.environ.get('CLAW_SCOPE_OUTSIDE')
            os.environ['CLAW_SCOPE_OUTSIDE'] = str(outside)
            try:
                self.assertEqual((f'{outside}/secret.txt',), extract_path_candidates('cat $CLAW_SCOPE_OUTSIDE/secret.txt'))
                decision = WorkspacePathScope.from_root(workspace).validate_payload('cat $CLAW_SCOPE_OUTSIDE/secret.txt')
            finally:
                if previous is None:
                    os.environ.pop('CLAW_SCOPE_OUTSIDE', None)
                else:
                    os.environ['CLAW_SCOPE_OUTSIDE'] = previous

            self.assertFalse(decision.allowed)
            self.assertIn(str(outside.resolve()), decision.resolved or '')

    def test_attached_shell_redirection_targets_are_validated(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workspace = root / 'workspace'
            outside = root / 'outside'
            workspace.mkdir()
            outside.mkdir()
            (outside / 'secret.txt').write_text('secret')

            self.assertEqual(
                ('../outside/secret.txt', '../outside/error.log'),
                extract_path_candidates(
                    'cat <../outside/secret.txt 2>../outside/error.log'
                ),
            )
            decision = WorkspacePathScope.from_root(workspace).validate_payload(
                'cat <../outside/secret.txt 2>../outside/error.log'
            )

            self.assertFalse(decision.allowed)
            self.assertIn(str(outside.resolve()), decision.resolved or '')

    def test_explicit_worktree_roots_are_allowed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workspace = root / 'workspace'
            worktree = root / 'worktree'
            workspace.mkdir()
            worktree.mkdir()
            (worktree / 'file.txt').write_text('ok')

            decision = WorkspacePathScope.from_roots((workspace, worktree)).validate_payload(f'cat {worktree}/file.txt')

            self.assertTrue(decision.allowed, decision.reason)

    def test_windows_absolute_paths_are_denied_for_posix_workspace(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / 'workspace'
            workspace.mkdir()

            drive_decision = WorkspacePathScope.from_root(workspace).validate_payload(r'type C:\Users\other\secret.txt')
            unc_decision = WorkspacePathScope.from_root(workspace).validate_payload(r'type \\server\share\secret.txt')

            self.assertFalse(drive_decision.allowed)
            self.assertIn('windows absolute path', drive_decision.reason)
            self.assertFalse(unc_decision.allowed)
            self.assertIn('windows absolute path', unc_decision.reason)

    def test_file_and_shell_tools_use_workspace_scope_context(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workspace = root / 'workspace'
            outside = root / 'outside'
            workspace.mkdir()
            outside.mkdir()
            context = ToolPermissionContext.from_iterables(workspace_root=workspace, cwd=workspace)

            file_result = execute_tool('FileReadTool', f'{outside}/secret.txt', permission_context=context)
            shell_result = execute_tool('BashTool', f'cat {outside}/secret.txt', permission_context=context)
            inside_result = execute_tool('FileReadTool', './allowed.txt', permission_context=context)

            self.assertFalse(file_result.handled)
            self.assertIn('Permission denied', file_result.message)
            self.assertFalse(shell_result.handled)
            self.assertIn('Permission denied', shell_result.message)
            self.assertTrue(inside_result.handled)

    def test_permission_denial_stream_events_expose_status_and_reason(self) -> None:
        engine = QueryEnginePort.from_workspace()
        denial = PermissionDenial('BashTool', 'path resolves outside workspace scope')

        events = list(engine.stream_submit_message('cat ../secret.txt', matched_tools=('BashTool',), denied_tools=(denial,)))
        permission_event = next(event for event in events if event['type'] == 'permission_denial')
        result = engine.submit_message('cat ../secret.txt', matched_tools=('BashTool',), denied_tools=(denial,))

        self.assertEqual('blocked', permission_event['denials'][0]['status'])
        self.assertEqual('path resolves outside workspace scope', permission_event['denials'][0]['reason'])
        self.assertIn('status=blocked', result.output)
        self.assertIn('path resolves outside workspace scope', result.output)


if __name__ == '__main__':
    unittest.main()
