import functools
import os
import shutil
import subprocess

# Warning: get_bash_executable and require_bash use @functools.lru_cache.
# Do not monkeypatch os.environ['PATH'] or shutil.which in tests and expect 
# these functions to re-evaluate. If you must patch them, call .cache_clear()
# before and after the test.

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
