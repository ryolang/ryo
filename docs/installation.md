# Installing Ryo

## Quick Install

**Linux / macOS / WSL:**
```bash
curl -fsSL https://ryolang.org/install.sh | sh
```

**Windows (PowerShell):**
```powershell
irm https://ryolang.org/install.ps1 | iex
```

After installation, restart your terminal and verify:
```bash
ryo --version
```

## What Gets Installed

The installer automatically sets up:
- Ryo compiler and toolchain in `~/.ryo/bin/`
- Zig linker (downloaded automatically) in `~/.ryo/tools/zig/`
- PATH configuration in your shell profile

All files are contained in the `~/.ryo/` directory for easy management.

## Tools Included

- **`ryo run`** - Compile and run programs
- **`ryo build`** - Compile to binaries
- **`ryo test`** - Run tests
- **`ryo fmt`** - Format code
- **`ryo upgrade`** - Update Ryo to the latest version

## Alternative Installation

### Manual Installation
1. Download the latest release from [GitHub Releases](https://github.com/ryolang/ryo/releases)
2. Extract to your preferred location
3. Add the `bin` directory to your PATH

### Build from Source
```bash
git clone https://github.com/ryolang/ryo.git
cd ryo
cargo build --release
```

## Troubleshooting

**Command not found:**
- Restart your terminal after installation
- Check that `~/.ryo/bin` is in your PATH


## Updating

Update to the latest version:
```bash
ryo upgrade
```

Or re-run the installation script.

## Uninstalling

```bash
rm -rf ~/.ryo
```

Remove the PATH entry from your shell profile (`.zshrc`, `.bashrc`, etc.).

## Next Steps

- **[Getting Started Guide](getting_started.md)** - Learn the language basics
- **[Package Manager Guide](pkg_manager.md)** - Manage dependencies