#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence


@dataclass(frozen=True)
class ProbeResult:
    kind: str
    argv: list[str]
    returncode: int | None
    stdout: bytes
    stderr: bytes
    message: str | None = None

    @property
    def stdout_text(self) -> str:
        return self.stdout.decode('utf-8', errors='replace')

    @property
    def stderr_text(self) -> str:
        return self.stderr.decode('utf-8', errors='replace')

    def to_json_dict(self) -> dict[str, object]:
        return {
            'kind': self.kind,
            'argv': self.argv,
            'returncode': self.returncode,
            'stdout': self.stdout_text,
            'stderr': self.stderr_text,
            'message': self.message,
        }


def run_probe(argv: Sequence[str], *, timeout: float = 10.0, require_stdout_json_byte0: bool = False) -> ProbeResult:
    explicit_argv = [str(arg) for arg in argv]
    if not explicit_argv:
        return ProbeResult(
            kind='probe_error',
            argv=[],
            returncode=None,
            stdout=b'',
            stderr=b'',
            message='argv must contain at least the executable path',
        )

    try:
        completed = subprocess.run(
            explicit_argv,
            capture_output=True,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        return ProbeResult(
            kind='timeout',
            argv=explicit_argv,
            returncode=None,
            stdout=exc.stdout or b'',
            stderr=exc.stderr or b'',
            message=f'probe timed out after {timeout:g}s',
        )
    except (OSError, ValueError) as exc:
        return ProbeResult(
            kind='probe_error',
            argv=explicit_argv,
            returncode=None,
            stdout=b'',
            stderr=b'',
            message=str(exc),
        )

    if require_stdout_json_byte0:
        if not completed.stdout:
            return ProbeResult(
                kind='product_error',
                argv=explicit_argv,
                returncode=completed.returncode,
                stdout=completed.stdout,
                stderr=completed.stderr,
                message='stdout is empty; expected JSON at byte 0',
            )
        if completed.stdout[:1] not in (b'{', b'['):
            return ProbeResult(
                kind='product_error',
                argv=explicit_argv,
                returncode=completed.returncode,
                stdout=completed.stdout,
                stderr=completed.stderr,
                message='stdout JSON does not start at byte 0',
            )
        try:
            json.loads(completed.stdout.decode('utf-8'))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            return ProbeResult(
                kind='product_error',
                argv=explicit_argv,
                returncode=completed.returncode,
                stdout=completed.stdout,
                stderr=completed.stderr,
                message=f'stdout is not parseable JSON: {exc}',
            )

    if completed.returncode != 0:
        return ProbeResult(
            kind='product_error',
            argv=explicit_argv,
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
            message=f'process exited with code {completed.returncode}',
        )

    return ProbeResult(
        kind='ok',
        argv=explicit_argv,
        returncode=completed.returncode,
        stdout=completed.stdout,
        stderr=completed.stderr,
    )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description='Run an argv-safe dogfood probe and emit separated channels as JSON.')
    parser.add_argument('--timeout', type=float, default=10.0)
    parser.add_argument('--stdout-json-byte0', action='store_true', help='Require stdout to be parseable JSON starting at byte 0.')
    parser.add_argument('command', nargs=argparse.REMAINDER, help='Executable and arguments to run. Use -- before the target argv.')
    args = parser.parse_args(argv)
    command = args.command
    if command and command[0] == '--':
        command = command[1:]

    result = run_probe(command, timeout=args.timeout, require_stdout_json_byte0=args.stdout_json_byte0)
    print(json.dumps(result.to_json_dict(), sort_keys=True))
    return 0 if result.kind == 'ok' else 1


if __name__ == '__main__':
    raise SystemExit(main())
