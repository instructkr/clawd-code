from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from .path_scope import PathScopeDecision, WorkspacePathScope


@dataclass(frozen=True)
class ToolPermissionContext:
    deny_names: frozenset[str] = field(default_factory=frozenset)
    deny_prefixes: tuple[str, ...] = ()
    workspace_scope: WorkspacePathScope | None = None
    cwd: Path | None = None

    @classmethod
    def from_iterables(
        cls,
        deny_names: list[str] | None = None,
        deny_prefixes: list[str] | None = None,
        workspace_root: str | Path | None = None,
        workspace_roots: list[str | Path] | tuple[str | Path, ...] | None = None,
        cwd: str | Path | None = None,
    ) -> 'ToolPermissionContext':
        roots: list[str | Path] = []
        if workspace_roots:
            roots.extend(workspace_roots)
        if workspace_root is not None:
            roots.append(workspace_root)
        return cls(
            deny_names=frozenset(name.lower() for name in (deny_names or [])),
            deny_prefixes=tuple(prefix.lower() for prefix in (deny_prefixes or [])),
            workspace_scope=WorkspacePathScope.from_roots(roots) if roots else None,
            cwd=Path(cwd).expanduser().resolve(strict=False) if cwd is not None else None,
        )

    def blocks(self, tool_name: str) -> bool:
        lowered = tool_name.lower()
        return lowered in self.deny_names or any(lowered.startswith(prefix) for prefix in self.deny_prefixes)

    def validate_payload_scope(self, tool_name: str, payload: str) -> PathScopeDecision:
        if self.workspace_scope is None or not _scope_checked_tool(tool_name):
            return PathScopeDecision(True, 'workspace path scope not required for this tool')
        return self.workspace_scope.validate_payload(payload, cwd=self.cwd)


def _scope_checked_tool(tool_name: str) -> bool:
    lowered = tool_name.lower()
    return any(marker in lowered for marker in ('bash', 'shell', 'powershell', 'fileread', 'filewrite', 'fileedit'))
