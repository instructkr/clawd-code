import functools
import os
import shutil
import subprocess
import unittest
from pathlib import Path


@functools.lru_cache(maxsize=1)
def get_bash_executable() -> str | None:
    if os.name == 'nt':
        for candidate in (
            r'C:\Program Files\Git\bin\bash.exe',
            r'C:\Program Files\Git\usr\bin\bash.exe',
            r'C:\Program Files (x86)\Git\bin\bash.exe',
            os.path.expandvars(r'%LOCALAPPDATA%\Programs\Git\bin\bash.exe'),
        ):
            if os.path.exists(candidate):
                return candidate
    bash = shutil.which('bash')
    if bash and 'WindowsApps' not in bash:
        return bash
    return None


@functools.lru_cache(maxsize=1)
def require_bash() -> bool:
    bash = get_bash_executable()
    if not bash:
        return False
    try:
        res = subprocess.run([bash, '-c', 'echo 1'], capture_output=True, text=True, timeout=2)
        return res.returncode == 0
    except Exception:
        return False


REPO_ROOT = Path(__file__).resolve().parents[1]
PRE_PUSH_HOOK = REPO_ROOT / '.github' / 'hooks' / 'pre-push'


class PrePushHookContractTests(unittest.TestCase):
    @unittest.skipUnless(require_bash(), 'Requires bash')
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

    @unittest.skipUnless(require_bash(), 'Requires bash')
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
