from __future__ import annotations

import glob
import os
import re
import shlex
from dataclasses import dataclass
from pathlib import Path, PureWindowsPath

_GLOB_META = set('*?[')
_WINDOWS_DRIVE_RE = re.compile(r'^[A-Za-z]:[\\/]')
_WINDOWS_UNC_RE = re.compile(r'^(?:\\\\|//)[^\\/]+[\\/][^\\/]+')
_ENV_ASSIGNMENT_RE = re.compile(r'^[A-Za-z_][A-Za-z0-9_]*=')
_REDIRECTION_TARGET_RE = re.compile(r'^(?:\d*)?(?:<>|>>?|<)(.+)$|^&>>?(.+)$')


@dataclass(frozen=True)
class PathScopeDecision:
    allowed: bool
    reason: str
    candidate: str | None = None
    resolved: str | None = None


@dataclass(frozen=True)
class WorkspacePathScope:
    """Validate tool/shell path operands against explicit workspace roots.

    The policy is intentionally conservative for the Python port: any candidate
    path that resolves outside the configured roots is denied, including paths
    reached through symlinks or glob expansion. Windows drive/UNC paths are
    treated as out-of-scope on POSIX roots unless an allowed root is also a
    Windows-style root with the same prefix.
    """

    roots: tuple[Path, ...]

    @classmethod
    def from_root(cls, root: str | Path) -> 'WorkspacePathScope':
        return cls.from_roots((root,))

    @classmethod
    def from_roots(cls, roots: tuple[str | Path, ...] | list[str | Path]) -> 'WorkspacePathScope':
        resolved_roots = tuple(Path(root).expanduser().resolve(strict=False) for root in roots)
        if not resolved_roots:
            raise ValueError('at least one workspace root is required')
        return cls(resolved_roots)

    def validate_payload(self, payload: str, cwd: str | Path | None = None) -> PathScopeDecision:
        cwd_path = Path(cwd).expanduser().resolve(strict=False) if cwd else self.roots[0]
        cwd_decision = self.validate_path(cwd_path)
        if not cwd_decision.allowed:
            return PathScopeDecision(False, f'cwd outside workspace scope: {cwd_path}', str(cwd_path), cwd_decision.resolved)
        for candidate in extract_path_candidates(payload):
            decision = self.validate_path(candidate, cwd_path)
            if not decision.allowed:
                return decision
        return PathScopeDecision(True, 'all path candidates are inside workspace scope')

    def validate_path(self, candidate: str | Path, cwd: str | Path | None = None) -> PathScopeDecision:
        raw = os.path.expandvars(os.path.expanduser(str(candidate)))
        if _is_windows_absolute(raw):
            return self._validate_windows_path(raw)
        base = Path(cwd).expanduser().resolve(strict=False) if cwd else self.roots[0]
        path = Path(raw)
        if not path.is_absolute():
            path = base / path
        expanded = self._expand_glob(path)
        for expanded_path in expanded:
            resolved = expanded_path.resolve(strict=False)
            if not any(_is_relative_to(resolved, root) for root in self.roots):
                return PathScopeDecision(
                    False,
                    'path resolves outside workspace scope',
                    str(candidate),
                    str(resolved),
                )
        return PathScopeDecision(True, 'path is inside workspace scope', str(candidate), str(expanded[0].resolve(strict=False)))

    def _expand_glob(self, path: Path) -> tuple[Path, ...]:
        path_text = str(path)
        if any(char in path_text for char in _GLOB_META):
            matches = tuple(Path(match) for match in glob.glob(path_text, recursive=True))
            if matches:
                return matches
            # For unmatched globs, validate the stable non-glob parent prefix.
            stable_parts: list[str] = []
            for part in path.parts:
                if any(char in part for char in _GLOB_META):
                    break
                stable_parts.append(part)
            if stable_parts:
                return (Path(*stable_parts),)
        return (path,)

    def _validate_windows_path(self, raw: str) -> PathScopeDecision:
        candidate = PureWindowsPath(raw)
        for root in self.roots:
            root_text = str(root)
            if not _is_windows_absolute(root_text):
                continue
            try:
                candidate.relative_to(PureWindowsPath(root_text))
                return PathScopeDecision(True, 'windows path is inside workspace scope', raw, str(candidate))
            except ValueError:
                continue
        return PathScopeDecision(False, 'windows absolute path is outside workspace scope', raw, str(candidate))


def extract_path_candidates(payload: str) -> tuple[str, ...]:
    """Return conservative path-like operands from a shell/tool payload."""

    try:
        tokens = shlex.split(payload, posix=True)
    except ValueError:
        tokens = payload.split()
    raw_tokens = payload.split()
    candidates: list[str] = []
    for token in (*tokens, *raw_tokens):
        if not token or token.startswith('-') or _ENV_ASSIGNMENT_RE.match(token):
            continue
        token = _strip_redirection_operator(token)
        expanded = os.path.expandvars(os.path.expanduser(token))
        if _looks_like_path(token) or _looks_like_path(expanded):
            candidate = expanded if _looks_like_path(expanded) else token
            if candidate not in candidates:
                candidates.append(candidate)
    return tuple(candidates)


def _looks_like_path(token: str) -> bool:
    return (
        token in {'.', '..'}
        or token.startswith(('./', '../', '/', '~/', '~/'))
        or '..' in token.split('/')
        or '/' in token
        or '\\' in token
        or any(char in token for char in _GLOB_META)
        or _is_windows_absolute(token)
    )


def _strip_redirection_operator(token: str) -> str:
    match = _REDIRECTION_TARGET_RE.match(token)
    if match is None:
        return token
    return next(group for group in match.groups() if group is not None)


def _is_windows_absolute(value: str) -> bool:
    return bool(_WINDOWS_DRIVE_RE.match(value) or _WINDOWS_UNC_RE.match(value))


def _is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False
