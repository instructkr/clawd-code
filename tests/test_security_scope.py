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


def _create_directory_link(target: Path, link: Path) -> None:
    """Create a directory symlink or fallback to an NTFS junction on Windows.

    Standard Windows user accounts cannot create symbolic links without
    SeCreateSymbolicLinkPrivilege (Developer Mode / Elevation), but NTFS
    directory junctions can be created unprivileged and exercise the exact same
    path resolution logic in Path.resolve().

    Note: NTFS junctions can only target local directory paths and cannot point
    at UNC/remote targets. Links resolving to remote/UNC paths are covered
    deterministically via `test_symlink_resolving_to_unc_escape_mocked`.
    """
    try:
        link.symlink_to(target, target_is_directory=True)
    except OSError as e:
        if getattr(e, 'winerror', None) == 1314 and os.name == 'nt':
            try:
                import _winapi
                _winapi.CreateJunction(str(target), str(link))
                return
            except Exception:
                pass
            self_skip_msg = 'Requires filesystem symlink or junction support on Windows runner'
            raise unittest.SkipTest(self_skip_msg) from e
        raise


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
            _create_directory_link(outside, link)

            decision = WorkspacePathScope.from_root(workspace).validate_payload('cat linked-outside/secret.txt')

            self.assertFalse(decision.allowed)
            self.assertIn(str(outside.resolve()), decision.resolved or '')

    def test_windows_absolute_symlink_escape_is_denied(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workspace = root / 'workspace'
            outside = root / 'outside'
            workspace.mkdir()
            outside.mkdir()
            (outside / 'secret.txt').write_text('secret')
            link = workspace / 'linked-outside'
            _create_directory_link(outside, link)

            payload = f'cat {link}/secret.txt'
            decision = WorkspacePathScope.from_root(workspace).validate_payload(payload)

            self.assertFalse(decision.allowed)
            self.assertIn('outside workspace scope', decision.reason)

    def test_symlink_resolution_escape_mocked(self) -> None:
        """Verify containment check catches escapes via resolve() even if unprivileged."""
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / 'workspace'
            workspace.mkdir()
            scope = WorkspacePathScope.from_root(workspace)

            from unittest.mock import patch
            fake_target = (Path(tmp) / 'outside' / 'secret.txt').resolve()
            with patch.object(Path, 'resolve', return_value=fake_target):
                decision = scope.validate_path(str(workspace / 'fake-link' / 'secret.txt'))
                self.assertFalse(decision.allowed)
                self.assertIn('outside workspace scope', decision.reason)

    def test_symlink_resolving_to_unc_escape_mocked(self) -> None:
        """Verify containment check denies links resolving to remote/UNC targets."""
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / 'workspace'
            workspace.mkdir()
            scope = WorkspacePathScope.from_root(workspace)

            from unittest.mock import patch
            unc_target = Path(r'\\remote-server\share\secret.txt')
            with patch.object(Path, 'resolve', return_value=unc_target):
                decision = scope.validate_path(str(workspace / 'net-link' / 'secret.txt'))
                self.assertFalse(decision.allowed)
                self.assertIn('outside workspace scope', decision.reason)

    def test_unresolvable_path_raises_oserror_is_denied(self) -> None:
        """Verify that paths raising OSError during resolve() are explicitly denied."""
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / 'workspace'
            workspace.mkdir()
            scope = WorkspacePathScope.from_root(workspace)

            from unittest.mock import patch
            with patch.object(Path, 'resolve', side_effect=OSError('dangling symlink or filesystem error')):
                decision = scope.validate_path(str(workspace / 'broken_link.txt'))
                self.assertFalse(decision.allowed)
                self.assertIn('cannot be resolved', decision.reason)

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

    def test_extract_path_candidates_preserves_unc_paths(self) -> None:
        payload = r'type \\server\share\secret.txt'
        candidates = extract_path_candidates(payload)
        self.assertIn(r'\\server\share\secret.txt', candidates)

    def test_windows_absolute_paths_are_denied_for_posix_workspace(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / 'workspace'
            workspace.mkdir()

            drive_decision = WorkspacePathScope.from_root(workspace).validate_payload(r'type C:\Users\other\secret.txt')
            unc_decision = WorkspacePathScope.from_root(workspace).validate_payload(r'type \\server\share\secret.txt')

            self.assertFalse(drive_decision.allowed)
            self.assertFalse(unc_decision.allowed)
            if os.name == 'nt':
                self.assertIn('outside workspace scope', drive_decision.reason)
                self.assertIn('outside workspace scope', unc_decision.reason)
            else:
                self.assertIn('windows absolute path', drive_decision.reason)
                self.assertIn('windows absolute path', unc_decision.reason)

    def test_drive_relative_paths_are_resolved_and_denied_if_cross_drive(self) -> None:
        """Verify that drive-relative paths like C:foo behave correctly."""
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / 'workspace'
            workspace.mkdir()
            scope = WorkspacePathScope.from_root(workspace)

            # A drive-relative path on a DIFFERENT drive acts like an absolute escape
            # If tmp is on C:, Z:foo resolves to Z:\foo which is outside.
            other_drive = 'Z:' if workspace.drive.upper() != 'Z:' else 'Y:'
            decision_cross = scope.validate_path(f'{other_drive}foo')
            
            # A drive-relative path on the SAME drive acts like a relative path to the CWD
            # C:foo in C:\workspace resolves to C:\workspace\foo
            decision_same = scope.validate_path(f'{workspace.drive}foo')
            
            if os.name == 'nt':
                self.assertFalse(decision_cross.allowed)
                self.assertIn('outside workspace scope', decision_cross.reason)
                self.assertTrue(decision_same.allowed)

    def test_unc_paths_are_evaluated_correctly_inside_and_outside(self) -> None:
        """Verify that UNC paths are properly identified and validated for containment."""
        with tempfile.TemporaryDirectory() as tmp:
            local_workspace = Path(tmp) / 'workspace'
            local_workspace.mkdir()

            # 1. Test extraction of escaped UNC paths (like from JSON payloads)
            payload_escaped = r'type \\\\server\\share\\secret.txt'
            candidates_escaped = extract_path_candidates(payload_escaped)
            self.assertIn(r'\\\\server\\share\\secret.txt', candidates_escaped)
            
            # 1b. Test extraction of unquoted UNC paths to ensure they survive shlex.split(posix=True)
            payload_unquoted = r'type \\server\share\secret.txt'
            candidates_unquoted = extract_path_candidates(payload_unquoted)
            self.assertIn(r'\\server\share\secret.txt', candidates_unquoted)

            # 2. Test denial of UNC path when workspace is on a local drive
            decision_outside = WorkspacePathScope.from_root(local_workspace).validate_payload(payload_escaped)
            self.assertFalse(decision_outside.allowed)
            
            # 3. Test allowance of UNC path when workspace root is itself a UNC path
            # We mock resolve() because resolving a fake UNC path might raise OSError or hang
            if os.name == 'nt':
                from unittest.mock import patch
                unc_workspace = Path(r'\\server\share\workspace')
                inside_payload = r"type '\\server\share\workspace\secret.txt'"
                
                def _fake_resolve(self, strict=False):
                    return self
                    
                with patch.object(Path, 'resolve', autospec=True, side_effect=_fake_resolve):
                    scope = WorkspacePathScope.from_root(unc_workspace)
                    decision_inside = scope.validate_payload(inside_payload)
                    self.assertTrue(decision_inside.allowed)

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
