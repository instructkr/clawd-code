import functools
import os
import shutil
import subprocess
import unittest
from pathlib import Path

from tests._bash import get_bash_executable, require_bash, bash_skip_reason


REPO_ROOT = Path(__file__).resolve().parents[1]
PRE_PUSH_HOOK = REPO_ROOT / '.github' / 'hooks' / 'pre-push'


class PrePushHookContractTests(unittest.TestCase):
    @unittest.skipUnless(require_bash(), bash_skip_reason())
    def test_skip_escape_hatch_exits_successfully_with_stderr_notice(self) -> None:
        bash_cmd = get_bash_executable() or 'bash'
        env = os.environ.copy()
        env['SKIP_CLAW_PRE_PUSH_BUILD'] = '1'

        result = subprocess.run(
            [bash_cmd, str(PRE_PUSH_HOOK)],
            cwd=REPO_ROOT,
            env=env,
            check=True,
            capture_output=True,
            text=True,
        )

        self.assertEqual('', result.stdout)
        self.assertIn('SKIP_CLAW_PRE_PUSH_BUILD=1', result.stderr)
        self.assertIn('skipping cargo workspace build', result.stderr)

    @unittest.skipUnless(require_bash(), bash_skip_reason())
    def test_default_build_gate_uses_workspace_locked_cargo_build(self) -> None:
        hook = PRE_PUSH_HOOK.read_text()

        self.assertIn(
            'cargo build --manifest-path rust/Cargo.toml --workspace --locked',
            hook,
        )
        self.assertIn(
            'build_cmd=(cargo build --manifest-path rust/Cargo.toml --workspace --locked)',
            hook,
        )


if __name__ == '__main__':
    unittest.main()
