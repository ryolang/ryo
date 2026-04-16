# Installation Architecture

Adhering to the **DX-First** philosophy, installation must be **instant, dependency-free, and isolated**.

Since Ryo depends on **Zig** for linking, the installation process has a critical job: **managing the Zig dependency automatically.** Asking a Python developer to "install Zig and add it to your path" before trying Ryo is unacceptable.

## 1. The Golden Standard: The One-Line Script

The primary method for 95% of users (Linux, macOS, WSL). Mirrors `rustup`, `bun`, and `deno`.

**The Command:**
```bash
curl -fsSL https://ryolang.org/install.sh | sh
```

**What the script does (`install.sh`):**
1.  **Detection:** Identifies OS (Linux/Darwin) and Arch (AMD64/ARM64).
2.  **Ryo Install:** Downloads the latest precompiled `ryo` binary to `~/.ryo/bin/`.
3.  **Zig Check:** Checks if `zig` is in `$PATH`.
    *   If `zig` is missing or the system version is incompatible, the script **downloads a local copy of Zig** to `~/.ryo/tools/zig/`.
4.  **Path Setup:** Appends `export PATH="$HOME/.ryo/bin:$PATH"` to `.zshrc` or `.bashrc`.
5.  **Config:** Creates a `~/.ryo/config.toml` pointing the Ryo compiler to the specific Zig binary downloaded.

## 2. The Windows Experience: PowerShell

Windows users are first-class citizens. WSL should not be required.

**The Command:**
```powershell
irm https://ryolang.org/install.ps1 | iex
```

**Behavior:** Same as the shell script, but modifies the User `PATH` environment variable in the Registry and installs to `%USERPROFILE%\.ryo\`.

---

## 3. Updates: `ryo upgrade`

Ryo includes a built-in self-update command, similar to `bun upgrade` or `deno upgrade`.

```bash
ryo upgrade
```

*   **Behavior:** Checks the latest release on GitHub/CDN, downloads the binary, and replaces the current executable in `~/.ryo/bin/`.
*   **Version Pinning:** `ryo upgrade v0.2.0` (Future)

---

## 4. Package Managers (Secondary)

Once v0.1 is stable, platform-native package managers should be supported for visibility.

*   **Homebrew (macOS/Linux):**
    *   Create a tap: `ryolang/ryo`.
    *   Formula: Depends on `zig` package.
    *   `brew install ryo`
*   **Winget (Windows):**
    *   Submit manifest to Microsoft.
    *   `winget install ryo`
*   **Docker:**
    *   Official image: `ryolang/ryo:latest`.
    *   Crucial for CI/CD pipelines.

---

## 5. Directory Structure (`~/.ryo/`)

Everything lives in one folder to keep the user's home directory clean.

```text
~/.ryo/
├── bin/
│   └── ryo           # The compiler executable
├── tools/
│   └── zig/          # Private copy of Zig (if system one is missing)
│       └── zig.exe
├── registry/         # Cache for downloaded packages
│   ├── index/
│   └── src/
└── config.toml       # User preferences (e.g., "linker_path = ...")
```

## 6. Roadmap Integration

A **Deployment** milestone is needed to ensure this is ready for launch.

**[NEW] Milestone 20.5: Distribution & Installer**
*   **Goal:** Zero-friction onboarding.
*   **Tasks:**
    *   **CI/CD:** GitHub Actions pipeline to build `ryo` for:
        *   `x86_64-unknown-linux-musl` (Static)
        *   `aarch64-unknown-linux-musl` (Static)
        *   `x86_64-apple-darwin`
        *   `aarch64-apple-darwin`
        *   `x86_64-pc-windows-msvc`
    *   **The Script:** Write `install.sh` and `install.ps1`.
    *   **Zig Logic:** Implement the logic to fetch the correct Zig tarball from `ziglang.org` JSON index if missing.
    *   **The Site:** A simple landing page with the `curl` command.

### Summary

1.  **Do not rely on the user** to install Zig. The installer downloads it automatically.
2.  **Start with `install.sh`** (Shell script) for v0.1. Cheap and effective.
3.  **Use `ryo upgrade`** for updates. Keep it simple.
4.  **Windows is critical:** The PowerShell script must work seamlessly on Day 1.
