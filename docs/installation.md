# Installing Ryo

This guide will help you install the Ryo programming language toolchain on your system.

## System Requirements

- **Operating System:** Linux, macOS, or Windows (with WSL recommended)
- **Architecture:** x86_64 or ARM64
- **Disk Space:** At least 500MB for compiler and standard library

## Installation Methods

### Using ryoinstall (Recommended)

The easiest way to install Ryo is via our installation script, which includes the compiler, standard library, and package manager.

```bash
# Linux / macOS / WSL
curl --proto '=https' --tlsv1.2 -sSf https://ryo-lang.org/install.sh | sh

# Windows (PowerShell)
# Invoke-RestMethod https://ryo-lang.org/install.ps1 | Invoke-Expression
```

Follow the on-screen instructions to add Ryo to your system's PATH. Verify the installation:

```bash
ryo --version
```

### Alternative Installation Methods

**Using Package Managers:**
```bash
# macOS with Homebrew
brew install ryo

# Ubuntu/Debian
sudo apt install ryo

# Arch Linux
pacman -S ryo

# Windows with Chocolatey
choco install ryo
```

**Manual Installation:**
1. Download the latest release from [GitHub Releases](https://github.com/ryolang/ryo/releases)
2. Extract the archive to your preferred location
3. Add the `bin` directory to your system's PATH

**Building from Source:**
```bash
git clone https://github.com/ryolang/ryo.git
cd ryo
cargo build --release
```

After installation, verify Ryo is working correctly:

```bash
# Check Ryo version
ryo --version

# Test with a simple program
echo 'fn main(): print("Hello, Ryo!")' > test.ryo
ryo run test.ryo
```

If you see "Hello, Ryo!" printed to the console, your installation is successful!

## Tools Included

The Ryo installation includes several tools:

- **`ryo`** - Main compiler and package manager
- **`ryo run`** - Compile and run Ryo programs
- **`ryo build`** - Compile programs to binaries
- **`ryo pkg`** - Package manager for dependencies
- **`ryo test`** - Run tests in your project
- **`ryo fmt`** - Format Ryo source code

## Troubleshooting

**Command not found:**
- Ensure Ryo's `bin` directory is in your PATH
- Restart your terminal after installation
- On Windows, check environment variables

**Permission denied:**
- On Unix systems, ensure the installer has execute permissions
- Try running the installer with `sudo` if needed

**Compilation errors:**
- Ensure you have a C linker installed (GCC or Clang)
- On Windows, install Visual Studio Build Tools

## Updating Ryo

To update to the latest version:

```bash
# If installed via ryoinstall
ryoinstall --update

# If installed via package manager
brew upgrade ryo        # macOS
sudo apt upgrade ryo    # Ubuntu/Debian
```

## Uninstalling

To remove Ryo from your system:

```bash
# If installed via ryoinstall
ryoinstall --uninstall

# Manual removal
rm -rf ~/.ryo           # Remove user data
# Remove from PATH in your shell profile
```

## Next Steps

Now that Ryo is installed, continue with:

- **[Getting Started Guide](getting_started.md)** - Learn the language fundamentals and write your first programs
- **[Package Manager Guide](pkg_manager.md)** - Learn how to manage dependencies and publish packages