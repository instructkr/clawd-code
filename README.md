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

You **do not** have to use the IDE. You can build and use the Rust engine purely as a CLI if you prefer.

---

## 🇹🇷 Türkçe Kurulum Rehberi

Bu proje, temel olarak 3 parçadan oluşur:
1. **Claw Motoru (`rust/`)**: Arka planda çalışan, yapay zeka ile haberleşen ana motor (Rust).
2. **Donanım Dinleyicisi (`hardware_daemon.py`)**: Ekran kartı sıcaklığı ve VRAM durumunu okuyan ufak python yazılımı.
3. **Masaüstü IDE (`tauri-ui/`)**: Arayüzü sağlayan program (Tauri + React).

Arayüzü kurmadan sadece kod (CLI) olarak da kullanabilirsiniz. Ancak tam teşekküllü IDE kurulumu için aşağıdaki adımları sırasıyla, eksiksiz yapmanız gerekmektedir.

### Bölüm 1: Gerekli Programların Kurulumu

#### Windows Kullanıcıları (ÖNEMLİ):
1. **C++ Build Tools Kurulumu:** Rust kodlarının derlenebilmesi için Windows C++ derleyicisine ihtiyacınız var.
   - [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) adresine gidin.
   - İndirip açtığınızda **"Desktop development with C++"** (C++ ile Masaüstü Geliştirme) kutucuğunu işaretleyin. (Yaklaşık 5-6 GB yer kaplar, kurulmasını bekleyin).
2. **Rust Kurulumu:** [rustup.rs](https://rustup.rs/) adresine gidin. Verilen `rustup-init.exe` dosyasını indirip çalıştırın. Kurulum bittikten sonra açık olan komut istemini (CMD/PowerShell) **kapatıp yeniden açın.**
3. **Node.js Kurulumu:** Arayüzün çalışması için gereklidir. [Node.js adresinden](https://nodejs.org/) v18 veya üstü bir sürümü kurun.
4. **Python Kurulumu:** [Python.org](https://www.python.org/downloads/) adresinden 3.9 veya üstü bir sürüm kurun. Kurarken **"Add Python to PATH"** seçeneğini işaretlemeyi unutmayın.

#### Linux Kullanıcıları (Ubuntu/Debian):
Terminali açıp şu komutu girin:
```text
sudo apt-get update
sudo apt-get install -y libglib2.0-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libwebkit2gtk-4.1-dev build-essential curl wget
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | shell
```

### Bölüm 2: Ana Motorun (Claw) Derlenmesi

İster terminalde (Powershell / CMD / Bash) komutlarla, isterseniz de VS Code'un terminali ile şu adımları izleyin:

```text
git clone https://github.com/ultraworkers/claw-code
cd claw-code/rust
cargo build --release
```

Bu işlem birkaç dakika sürebilir. Bittiğinde:
- **Windows:** `rust\target\release\claw.exe` adında bir dosya oluşacaktır.
- **Linux:** `rust/target/release/claw` adında bir dosya oluşacaktır.

*(Arayüzsüz kullanmak isterseniz sadece bu dosyayı terminalde çağırıp kullanabilirsiniz.)*

### Bölüm 3: Donanım Dinleyicisini Hazırlama

Python kütüphanesini yüklemek için projenin ana klasöründe (claw-code içinde) şu komutu çalıştırın:
```text
pip install pynvml
```
*(Eğer "pip tanınmıyor" hatası alırsanız `python -m pip install pynvml` veya `python3 -m pip install pynvml` deneyin).*

### Bölüm 4: Masaüstü IDE'nin Hazırlanması (Yan Uygulamaları Bağlama)

Masaüstü uygulamamızın, bizim bir üst adımda ürettiğimiz `claw.exe` ve `hardware_daemon.py` programlarına ulaşabilmesi gerekir. Bunun için özel bir `binaries` (çalıştırılabilir dosyalar) klasörü açıp içine kopyalamalıyız.

Bu dosyaların sonuna, işletim sisteminizin mimari adını eklemeniz **şarttır** (örneğin: `x86_64-pc-windows-msvc`). Ayrıca Tauri, yan uygulamaların (sidecar) Windows'ta bile sonlarında **`.exe` uzantısının OLMAMASINI** bekler.

**Windows için (Powershell'de çalıştırın):**
```powershell
# Ana klasörde olduğunuzdan emin olun (claw-code içindeyiz)
mkdir tauri-ui\src-tauri\binaries

# Claw motorunu kopyala ve ismini değiştir
copy rust\target\release\claw.exe tauri-ui\src-tauri\binaries\claw-x86_64-pc-windows-msvc
ren tauri-ui\src-tauri\binaries\claw-x86_64-pc-windows-msvc.exe claw-x86_64-pc-windows-msvc

# Donanım dinleyicisini (Python) derle ve kopyala
pip install pyinstaller pynvml
pyinstaller --onefile --name hardware_daemon hardware_daemon.py
mkdir tauri-ui\src-tauri\binaries
copy dist\hardware_daemon.exe tauri-ui\src-tauri\binaries\hardware_daemon-x86_64-pc-windows-msvc
# Tauri, yan uygulamaların uzantısız olmasını bekler, bu yüzden .exe uzantısını kaldırmalıyız
ren tauri-ui\src-tauri\binaries\hardware_daemon-x86_64-pc-windows-msvc.exe hardware_daemon-x86_64-pc-windows-msvc
```

**Linux / macOS (Intel) için:**
```text
# Ana klasörden
mkdir -p tauri-ui/src-tauri/binaries

cp rust/target/release/claw tauri-ui/src-tauri/binaries/claw-x86_64-unknown-linux-gnu

# Donanım dinleyicisini (Python) derle ve kopyala
pip install pyinstaller pynvml
pyinstaller --onefile --name hardware_daemon hardware_daemon.py
cp dist/hardware_daemon tauri-ui/src-tauri/binaries/hardware_daemon-x86_64-unknown-linux-gnu
```

### Bölüm 5: IDE'yi Başlatma

Artık tüm parçalar hazır. Tauri arayüzüne girip uygulamanızı başlatın:

```text
cd tauri-ui
npm install

# Geliştirme modunda (pencereli) açmak için:
npm run tauri dev

# Tamamen paketlenmiş bir masaüstü uygulaması (.exe, .deb) oluşturmak için:
npm run tauri build
```

Uygulama açıldığında sol alttaki Ayarlar ikonuna tıklayarak Ollama bağlantınızı (`http://127.0.0.1:11434/v1`) veya API keyinizi girebilirsiniz.

---
---

## 🇬🇧 English Installation Guide

Our setup relies on three distinct pieces:
1. **Claw Engine (`rust/`)**: The source of truth for all AI logic and task execution.
2. **Hardware Telemetry (`hardware_daemon.py`)**: A standalone Python daemon checking GPU thermals via `pynvml`.
3. **Desktop IDE (`tauri-ui/`)**: A React/Vite/Tailwind frontend running on Tauri that interacts with the engine and telemetry as sidecars.

You **do not** have to use the IDE. You can build and use the Rust engine purely as a CLI if you prefer.

### Part 1: Install Dependencies

**Linux (Ubuntu/Debian):**
To compile Tauri and the Rust engine, you will need build-essential and GTK/Webkit libraries:
```text
sudo apt-get update
sudo apt-get install -y libglib2.0-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libwebkit2gtk-4.1-dev build-essential curl wget
```

**Windows:**
Install the [C++ Build Tools for Visual Studio](https://visualstudio.microsoft.com/visual-cpp-build-tools/).
Make sure you have "Desktop development with C++" selected during installation.

**All Platforms:**
1. **Rust**: Install via rustup: `curl --proto '=https' --tlsv1.2 -sSf https://rustup.rs | shell`
2. **Node.js**: Install Node 18+ (we recommend [nvm](https://github.com/nvm-sh/nvm))
3. **Python**: Install Python 3.9+

### Part 2: Build the Core CLI Engine

```text
git clone https://github.com/ultraworkers/claw-code
cd claw-code/rust
cargo build --release
```

Once built, the binary lives at `rust/target/release/claw` (or `claw.exe` on Windows). You can use it directly in your terminal if you don't want the IDE.

### Part 3: Install the Hardware Daemon Dependencies

```text
cd claw-code
pip install pynvml
```

### Part 4: Build and Run the IDE (Tauri)

The Desktop IDE requires the compiled `claw` engine and `hardware_daemon.py` to be available to it.
We need to copy the compiled binary to the Tauri app's expected sidecar directory. Tauri expects sidecars to have the target triple appended. **NOTE: Tauri requires sidecars to NOT have a `.exe` extension**, even on Windows.

**Windows:**
```powershell
mkdir tauri-ui\src-tauri\binaries
copy rust\target\release\claw.exe tauri-ui\src-tauri\binaries\claw-x86_64-pc-windows-msvc.exe
# Compile and copy the hardware daemon (Python)
pip install pyinstaller pynvml
pyinstaller --onefile --name hardware_daemon hardware_daemon.py
copy dist\hardware_daemon.exe tauri-ui\src-tauri\binaries\hardware_daemon-x86_64-pc-windows-msvc.exe
```

**Linux / macOS (Intel/AMD):**
```text
mkdir -p tauri-ui/src-tauri/binaries
cp rust/target/release/claw tauri-ui/src-tauri/binaries/claw-x86_64-unknown-linux-gnu
# Compile and copy the hardware daemon (Python)
pip install pyinstaller pynvml
pyinstaller --onefile --name hardware_daemon hardware_daemon.py
cp dist/hardware_daemon tauri-ui/src-tauri/binaries/hardware_daemon-x86_64-unknown-linux-gnu
```

### Part 5: Run

```text
cd tauri-ui
npm install

# Run in development mode
npm run tauri dev

# Or build the final application (AppImage, deb, exe, etc)
npm run tauri build
```
