# Claw Code & IDE

<p align="center">
  <a href="https://github.com/ultraworkers/claw-code">ultraworkers/claw-code</a>
  ·
  <a href="./USAGE.md">Usage</a>
  ·
  <a href="./rust/README.md">Rust workspace</a>
  ·
  <a href="./PARITY.md">Parity</a>
  ·
  <a href="./ROADMAP.md">Roadmap</a>
  ·
  <a href="https://discord.gg/5TUQKqFWd">UltraWorkers Discord</a>
</p>

Claw Code is the public Rust implementation of the `claw` CLI agent harness, now accompanied by a **Desktop IDE (Tauri)** and **Hardware Telemetry sidecar (Python)**.
The canonical Rust implementation lives in [`rust/`](./rust), and the new desktop wrapper lives in [`tauri-ui/`](./tauri-ui).

## System Architecture

Our setup relies on three distinct pieces:
1. **Claw Engine (`rust/`)**: The source of truth for all AI logic and task execution.
2. **Hardware Telemetry (`hardware_daemon.py`)**: A standalone Python daemon checking GPU thermals via `pynvml` or `rocm-smi`.
3. **Desktop IDE (`tauri-ui/`)**: A React/Vite/Tailwind frontend running on Tauri that interacts with the engine and telemetry as sidecars.

You **do not** have to use the IDE. You can build and use the Rust engine purely as a CLI if you prefer.

---

## 🛠️ Detailed Installation Instructions

### Part 1: Install Dependencies

**Linux (Ubuntu/Debian):**
To compile Tauri and the Rust engine, you will need build-essential and GTK/Webkit libraries:
```bash
sudo apt-get update
sudo apt-get install -y libglib2.0-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libwebkit2gtk-4.1-dev build-essential curl wget
```

**Windows:**
Install the [C++ Build Tools for Visual Studio](https://visualstudio.microsoft.com/visual-cpp-build-tools/).
Make sure you have "Desktop development with C++" selected during installation.

**All Platforms:**
1. **Rust**: Install via rustup: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
2. **Node.js**: Install Node 18+ (we recommend [nvm](https://github.com/nvm-sh/nvm))
3. **Python**: Install Python 3.9+

---

### Part 2: Build the Core CLI Engine (Optional standalone)

If you only want the CLI, or want to compile the sidecar for the IDE:

```bash
git clone https://github.com/ultraworkers/claw-code
cd claw-code/rust
cargo build --release
```

**Is it possible to use without the IDE?**
Yes. Once built, the binary lives at `rust/target/release/claw` (or `claw.exe` on Windows). You can use it directly in your terminal:
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
./rust/target/release/claw prompt "say hello"
```

---

### Part 3: Install the Hardware Daemon Dependencies

The hardware daemon is required for the IDE's system monitor to show GPU stats.

```bash
cd claw-code
pip install pynvml
# Optional: create a virtual environment first
# python -m venv venv && source venv/bin/activate
```

---

### Part 4: Build and Run the IDE (Tauri)

The Desktop IDE requires the compiled `claw` engine and `hardware_daemon.py` to be available to it.
We need to copy the compiled binary to the Tauri app's expected sidecar directory.

**1. Copy the Claw binary:**
```bash
# From the root of the repository
mkdir -p tauri-ui/src-tauri/binaries

# Linux / macOS (Intel/AMD)
cp rust/target/release/claw tauri-ui/src-tauri/binaries/claw-x86_64-unknown-linux-gnu

# Windows
# copy rust\target\release\claw.exe tauri-ui\src-tauri\binaries\claw-x86_64-pc-windows-msvc.exe
```
*(Note: Replace the target triple `x86_64-unknown-linux-gnu` with your actual host architecture triple).*

**2. Make the python daemon executable (Linux/macOS):**
```bash
chmod +x hardware_daemon.py
cp hardware_daemon.py tauri-ui/src-tauri/binaries/hardware_daemon-x86_64-unknown-linux-gnu
```

**3. Install Node dependencies and start the IDE:**
```bash
cd tauri-ui
npm install

# Run in development mode
npm run tauri dev

# Or build the final application (AppImage, deb, exe, etc)
npm run tauri build
```

The IDE will launch and immediately connect to your local Ollama models (if configured) or standard API keys via the Settings Modal in the bottom left corner.

## Troubleshooting

- **IDE fails to find `claw` or `hardware_daemon`:** Tauri looks for sidecars matching the exact platform target triple (e.g. `claw-aarch64-apple-darwin`). Make sure the binaries in `tauri-ui/src-tauri/binaries/` have the correct suffix.
- **Hardware Monitor is empty/red:** Make sure `pynvml` is installed in the global python environment, or adjust the `hardware_daemon` to use your active virtual environment Python.
- **Ollama models not responding:** In the IDE Settings, ensure the Open AI Base URL is set to `http://127.0.0.1:11434/v1` and the model name matches your local model precisely (e.g. `llama3`).

