# Windows install and release quickstart

This page is the PowerShell-first path for installing, verifying, and safely switching providers on Windows. It is intentionally copyable without embedding live secrets.

## Choose an install path

### Option A: build from source in PowerShell

Use this when you are developing Claw Code or testing a local checkout.

```powershell
git clone https://github.com/ultraworkers/claw-code
Set-Location .\claw-code\rust
cargo build --workspace
.\target\debug\claw.exe --help
.\target\debug\claw.exe doctor
```

For an optimized local binary:

```powershell
Set-Location .\claw-code\rust
cargo build --workspace --release
.\target\release\claw.exe --help
```

### Option B: use a release artifact

Use this when a GitHub release publishes a Windows artifact. The release workflow publishes `claw-windows-x64.exe` plus `claw-windows-x64.exe.sha256`; if a future release wraps the binary in a ZIP, prefer the `windows-x86_64` / `pc-windows-msvc` asset and its matching checksum file.

```powershell
$Asset = "claw-windows-x64.exe"
$InstallRoot = "$env:LOCALAPPDATA\Programs\claw"
New-Item -ItemType Directory -Force $InstallRoot | Out-Null

# Download $Asset and $Asset.sha256 from the release page, then verify them:
$Actual = (Get-FileHash ".\$Asset" -Algorithm SHA256).Hash.ToLowerInvariant()
$Expected = (Get-Content ".\$Asset.sha256" | Select-Object -First 1).Split()[0].ToLowerInvariant()
if ($Actual -ne $Expected) { throw "checksum mismatch for $Asset" }

Copy-Item ".\$Asset" "$InstallRoot\claw.exe" -Force
& "$InstallRoot\claw.exe" --help
& "$InstallRoot\claw.exe" doctor
```

To make that binary available in new PowerShell windows:

```powershell
$InstallRoot = "$env:LOCALAPPDATA\Programs\claw"
[Environment]::SetEnvironmentVariable(
  "Path",
  [Environment]::GetEnvironmentVariable("Path", "User") + ";$InstallRoot",
  "User"
)
```

Open a new terminal before running `claw --help` from another directory.

### Option C: WSL

The repository `install.sh` path is for Linux, macOS, and Windows via WSL. Run it from inside your WSL distribution, not from native PowerShell:

```powershell
wsl --install
wsl
```

Then inside WSL:

```bash
git clone https://github.com/ultraworkers/claw-code
cd claw-code
./install.sh
```

## First-run health checks

Run these before using live prompts:

```powershell
Set-Location .\claw-code\rust
.\target\debug\claw.exe --help
.\target\debug\claw.exe doctor
.\target\debug\claw.exe status --output-format json
.\target\debug\claw.exe config --output-format json
```

`doctor`, `status`, `config`, and `version` support `--output-format json`; do not use a separate `--json` suffix.

## Safe credential setup

Set keys only in your local environment or a private `.env` file. Do not paste real keys into shell history shared with others, issue trackers, or documentation.

Current PowerShell session only:

```powershell
$env:ANTHROPIC_API_KEY = "sk-ant-REPLACE_ME"
```

Persist for future PowerShell windows:

```powershell
setx ANTHROPIC_API_KEY "sk-ant-REPLACE_ME"
```

Open a new terminal after `setx`. To remove a session-local key while testing provider switching:

```powershell
Remove-Item Env:\ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
```

## Safe provider switching examples

Provider routing is model-prefix first. When multiple credentials exist, choose an explicit model prefix so `claw` does not infer the wrong backend.

### Anthropic direct

```powershell
$env:ANTHROPIC_API_KEY = "sk-ant-REPLACE_ME"
Remove-Item Env:\OPENAI_BASE_URL -ErrorAction SilentlyContinue
Remove-Item Env:\OPENAI_API_KEY -ErrorAction SilentlyContinue

.\target\debug\claw.exe --model "sonnet" prompt "reply with ready"
```

### OpenAI-compatible gateway or OpenRouter

```powershell
Remove-Item Env:\ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
$env:OPENAI_BASE_URL = "https://openrouter.ai/api/v1"
$env:OPENAI_API_KEY = "sk-or-v1-REPLACE_ME"

.\target\debug\claw.exe --model "openai/gpt-4.1-mini" prompt "reply with ready"
```

For the default OpenAI-compatible API, omit `OPENAI_BASE_URL` or set it to `https://api.openai.com/v1`, and keep the `openai/` or `gpt-` model prefix explicit.

### Local OpenAI-compatible server

Use a loopback URL and a placeholder token unless your local server requires a real one:

```powershell
Remove-Item Env:\ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
$env:OPENAI_BASE_URL = "http://127.0.0.1:11434/v1"
$env:OPENAI_API_KEY = "local-dev-token"

.\target\debug\claw.exe --model "llama3.2" prompt "reply with ready"
```

If the local server is authless, remove `OPENAI_API_KEY` instead of putting a real cloud key into local testing:

```powershell
Remove-Item Env:\OPENAI_API_KEY -ErrorAction SilentlyContinue
```

### DashScope / Qwen

```powershell
Remove-Item Env:\ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
$env:DASHSCOPE_API_KEY = "sk-REPLACE_ME"

.\target\debug\claw.exe --model "qwen-plus" prompt "reply with ready"
```

## Windows and WSL notifications

Notification support is exposed through the `notifications` slash command in the interactive REPL. Use JSON/status commands first to confirm the CLI runs, then configure notifications from the REPL if your workflow needs them.

Native PowerShell smoke path:

```powershell
Set-Location .\claw-code\rust
.\target\debug\claw.exe
# inside the REPL:
/notifications
```

WSL smoke path:

```bash
cd claw-code/rust
./target/debug/claw
# inside the REPL:
/notifications
```

When moving between PowerShell and WSL, keep provider keys in the environment where `claw` is actually running; Windows user env vars set with `setx` are not automatically the same as WSL shell exports.

## Troubleshooting checklist

- `claw` not found: use `claw.exe` on Windows or run the binary by full path (`.\target\debug\claw.exe`).
- `cargo` not found: reopen PowerShell after installing Rust from <https://rustup.rs/>.
- `401 Invalid bearer token`: put `sk-ant-*` values in `ANTHROPIC_API_KEY`, not `ANTHROPIC_AUTH_TOKEN`.
- Wrong provider selected: add an explicit model prefix such as `openai/gpt-4.1-mini`, `qwen-plus`, or `grok`.
- Release ZIP extracted but command still fails: open a new terminal after updating the user `Path`, or call `& "$env:LOCALAPPDATA\Programs\claw\claw.exe"` directly.
