# G009 Windows docs/release readiness verification map

## Scope and source

This map ties the Stream 8 acceptance target from `.omx/plans/claw-code-2-0-adaptive-plan.md` to repository artifacts and local verification. It is the worker-1 integration lane artifact; it does not mutate `.omx/ultragoal` and avoids duplicating peer implementation lanes for Windows CI, install/provider docs, and policy/link work.

Stream 8 source requirement summary:

- PowerShell-first docs and CLI examples.
- Safe provider switching examples.
- Staged packaging path: source-only alpha first, binary release matrix next, package managers later.
- Windows smoke CI for help/doctor/config/status without live credentials.
- License, contribution, security, and support policies.
- Command/link validation for adoption docs.

## Acceptance-to-evidence matrix

| Acceptance area | Repository artifact(s) | Verification command(s) | Notes |
|---|---|---|---|
| PowerShell-first Windows install/run path | `README.md` (`Windows setup`, post-build binary location, PowerShell `.exe` examples); `install.sh` (Unix/WSL installer guard) | `python3 .github/scripts/check_doc_source_of_truth.py`; `cargo run -p rusty-claude-cli -- --help` | Current docs explicitly present Windows as a supported PowerShell path for source builds and `claw.exe`; `install.sh` is Linux/macOS/WSL-oriented, so native PowerShell binary usage and WSL installer usage must stay clearly separated. |
| Safe provider switching examples | `USAGE.md` (`Auth`, `Local Models`, `Supported Providers & Models`); `docs/MODEL_COMPATIBILITY.md` | `cargo test -p api providers::`; `cargo test -p rusty-claude-cli --test output_format_contract provider_diagnostics_explain_openai_compatible_capabilities -- --nocapture` | Provider docs cover Anthropic API-key vs bearer-token shape, OpenAI-compatible routing, Ollama/OpenRouter/DashScope examples, and prefix routing to avoid ambient credential misrouting. |
| Release artifact quickstart and staged packaging path | `README.md` (`Quick start`, `Post-build: locate the binary and verify`); `.github/workflows/release.yml`; `docs/windows-install-release.md` | `cargo build --release -p rusty-claude-cli`; `cargo run -p rusty-claude-cli -- version --output-format json`; `python3 .github/scripts/check_release_readiness.py (release-readiness gate)` | Release workflow packages Linux, macOS, and `claw-windows-x64.exe` assets with `.sha256` checksum files. README remains source-build-first, and the Windows quickstart names the checksum verification path. |
| Windows smoke CI without live credentials | `.github/workflows/rust-ci.yml`; CLI local-only surfaces in `rust/crates/rusty-claude-cli/src/main.rs` (`help`, `doctor`, resumed `/config`, `status`) | `cargo run -p rusty-claude-cli -- --help`; `cargo run -p rusty-claude-cli -- doctor --output-format json`; `cargo run -p rusty-claude-cli -- status --output-format json`; `cargo run -p rusty-claude-cli -- config --output-format json` | The smoke target is local-only command execution with isolated config and no real provider credentials. If the Windows CI lane is not present in a branch, this map is the integration checklist for that lane. |
| License metadata | `rust/Cargo.toml` (`workspace.package.license = "MIT"`) | `grep -n '^license = "MIT"' rust/Cargo.toml` | Cargo metadata declares MIT. A root `LICENSE` file remains the user-facing policy artifact to add if not already present in the policy lane. |
| Contribution/security/support policies | Expected root policy docs: `CONTRIBUTING.md`, `SECURITY.md`, `SUPPORT.md`; existing support links in `README.md` | `test -f CONTRIBUTING.md`; `test -f SECURITY.md`; `test -f SUPPORT.md`; `python3 .github/scripts/check_doc_source_of_truth.py` | These files are policy-lane outputs. This map records the exact release gate so missing files fail visibly instead of being inferred from README links. |
| Command/link validation | `.github/scripts/check_doc_source_of_truth.py`; `README.md`; `USAGE.md`; `docs/**` | `python3 .github/scripts/check_doc_source_of_truth.py`; `python3 - <<'PY' ...` link/reference check listed below | Existing validation catches stale branding/assets/invites across adoption docs. The lightweight reference check below catches broken relative Markdown links without network access. |

## Windows/local smoke command contract

Use isolated config and no live credentials. These commands must not require `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, or `DASHSCOPE_API_KEY`:

```powershell
# From repository root on Windows PowerShell
$env:CLAW_CONFIG_HOME = Join-Path $env:TEMP "claw-smoke-config"
Remove-Item Env:\ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
Remove-Item Env:\ANTHROPIC_AUTH_TOKEN -ErrorAction SilentlyContinue
Remove-Item Env:\OPENAI_API_KEY -ErrorAction SilentlyContinue
Remove-Item Env:\XAI_API_KEY -ErrorAction SilentlyContinue
Remove-Item Env:\DASHSCOPE_API_KEY -ErrorAction SilentlyContinue
cd rust
cargo run -p rusty-claude-cli -- --help
cargo run -p rusty-claude-cli -- doctor --output-format json
cargo run -p rusty-claude-cli -- status --output-format json
cargo run -p rusty-claude-cli -- config --output-format json
```

Equivalent Unix smoke used by this worker:

```bash
env -u ANTHROPIC_API_KEY -u ANTHROPIC_AUTH_TOKEN -u OPENAI_API_KEY -u XAI_API_KEY -u DASHSCOPE_API_KEY \
  CLAW_CONFIG_HOME="$(mktemp -d)" cargo run -p rusty-claude-cli -- --help
```

## Offline Markdown reference check

```bash
python3 - <<'PY'
from pathlib import Path
import re, sys
root = Path.cwd()
errors = []
for path in [Path('README.md'), Path('USAGE.md'), Path('PARITY.md'), Path('PHILOSOPHY.md'), *Path('docs').glob('*.md')]:
    if not path.exists():
        continue
    text = path.read_text(encoding='utf-8')
    for match in re.finditer(r'\[[^\]]+\]\(([^)]+)\)', text):
        target = match.group(1).split('#', 1)[0]
        if not target or '://' in target or target.startswith('mailto:'):
            continue
        if not (root / path.parent / target).resolve().exists():
            line = text.count('\n', 0, match.start()) + 1
            errors.append(f'{path}:{line}: missing relative link target {match.group(1)}')
if errors:
    print('\n'.join(errors))
    sys.exit(1)
print('offline markdown reference check passed')
PY
```

## Release gate

A Stream 8 release candidate is ready when all of the following are true:

1. PowerShell examples in `README.md` build and run `claw.exe` from a clean Windows checkout.
2. Provider examples in `USAGE.md` show session-local/shell-local switching, include cleanup for conflicting ambient credentials (`unset` / `Remove-Item Env:`), and never instruct users to paste secrets into persistent config by default.
3. Windows smoke CI runs help/doctor/config/status without live credentials, separates native PowerShell `claw.exe` smoke from WSL `install.sh` smoke, and archives JSON output on failure.
4. Release artifacts include the documented platform matrix or the docs clearly state source-only alpha status.
5. `LICENSE`, `CONTRIBUTING.md`, `SECURITY.md`, and `SUPPORT.md` exist or the policy lane records an explicit release-blocking exception.
6. Doc source-of-truth and offline relative-link validation pass.
