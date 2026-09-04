import functools
import os
import shutil
import subprocess

# Warning: _check_bash_state uses @functools.lru_cache.
# Do not monkeypatch os.environ['PATH'] or shutil.which in tests and expect 
# these functions to re-evaluate. If you must patch them, call .cache_clear()
# before and after the test, or use the _clear_bash_cache pytest fixture.

def _probe_bash(bash_path: str) -> bool:
    try:
        res = subprocess.run([bash_path, '-c', 'echo 1'], capture_output=True, text=True, timeout=2)
        return res.returncode == 0
    except Exception:
        return False

@functools.lru_cache(maxsize=1)
def _check_bash_state() -> tuple[str | None, str]:
    if os.name == 'nt':
        for candidate in (
            r'C:\Program Files\Git\bin\bash.exe',
            r'C:\Program Files\Git\usr\bin\bash.exe',
            r'C:\Program Files (x86)\Git\bin\bash.exe',
            os.path.expandvars(r'%LOCALAPPDATA%\Programs\Git\bin\bash.exe'),
        ):
            if os.path.exists(candidate):
                if _probe_bash(candidate):
                    return candidate, ""
                return None, f"bash present at {candidate} but unusable"
    bash = shutil.which('bash')
    if bash and 'WindowsApps' not in bash:
        if _probe_bash(bash):
            return bash, ""
        return None, f"bash present at {bash} but unusable"
    return None, "Requires bash"

def get_bash_executable() -> str | None:
    return _check_bash_state()[0]

def require_bash() -> bool:
    return get_bash_executable() is not None

def bash_skip_reason() -> str:
    return _check_bash_state()[1]
