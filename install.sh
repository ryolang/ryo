#!/bin/sh
set -e

# Configuration
REPO="pepe/ryox"
RELEASE_TAG="nightly"
INSTALL_DIR="$HOME/.ryo/bin"
BINARY_NAME="ryo"
GITHUB_API="https://api.github.com/repos/${REPO}/releases/tags/${RELEASE_TAG}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Parse arguments
PREFIX=""
FORCE=false
HELP=false
DRY_RUN=false

for arg in "$@"; do
    case "$arg" in
        --prefix=*)
            PREFIX="${arg#*=}"
            ;;
        --force)
            FORCE=true
            ;;
        --dry-run)
            DRY_RUN=true
            ;;
        --help|-h)
            HELP=true
            ;;
        *)
            echo "Unknown argument: $arg"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

show_help() {
    echo "Usage: $(basename "$0") [OPTIONS]"
    echo ""
    echo "Installs the latest Ryo nightly build from GitHub."
    echo ""
    echo "Options:"
    echo "  --prefix=DIR    Install to DIR instead of ~/.ryo/bin"
    echo "  --force        Overwrite existing installation"
    echo "  --dry-run      Show what would be done without making changes"
    echo "  --help, -h     Show this help message"
    echo ""
    echo "Environment variables:"
    echo "  RYO_REPO       GitHub repository (default: pepe/ryox)"
    echo "  RYO_RELEASE    Release tag (default: nightly)"
    echo ""
    exit 0
}

if [ "$HELP" = true ]; then
    show_help
fi

# Allow override via environment
REPO="${RYO_REPO:-$REPO}"
RELEASE_TAG="${RYO_RELEASE:-$RELEASE_TAG}"

# Determine OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

# Map OS and architecture to our naming scheme
case "$OS" in
    Linux*)
        PLATFORM="linux"
        ;;
    Darwin*)
        PLATFORM="macos"
        ;;
    *)
        echo "${RED}Unsupported OS: $OS${NC}" >&2
        echo "Supported platforms: Linux, macOS" >&2
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64|amd64)
        ARCH_NAME="x64"
        ;;
    aarch64|arm64)
        ARCH_NAME="arm64"
        ;;
    *)
        echo "${RED}Unsupported architecture: $ARCH${NC}" >&2
        echo "Supported architectures: x86_64, aarch64" >&2
        exit 1
        ;;
esac

# Map platform and architecture to target
if [ "$PLATFORM" = "macos" ] && [ "$ARCH_NAME" = "arm64" ]; then
    TARGET="ryo-macos-arm64"
elif [ "$PLATFORM" = "linux" ] && [ "$ARCH_NAME" = "x64" ]; then
    TARGET="ryo-linux-x64"
else
    echo "${RED}No build available for ${PLATFORM}-${ARCH_NAME}${NC}" >&2
    echo "Supported platforms: Linux (x64), macOS (ARM64/Apple Silicon)" >&2
    exit 1
fi

echo "${YELLOW}Detected platform: ${PLATFORM}-${ARCH_NAME}${NC}"
echo "${YELLOW}Target: ${TARGET}${NC}"

if [ -n "$PREFIX" ]; then
    INSTALL_DIR="$PREFIX"
fi

if [ "$DRY_RUN" = false ]; then
    mkdir -p "$INSTALL_DIR"
fi

BINARY_PATH="$INSTALL_DIR/$BINARY_NAME"

if [ -f "$BINARY_PATH" ]; then
    if [ "$FORCE" = false ]; then
        CURRENT_VERSION=$($BINARY_PATH --version 2>/dev/null || echo "unknown")
        echo "${YELLOW}Warning: Ryo is already installed at $BINARY_PATH${NC}"
        echo "  Current version: $CURRENT_VERSION"
        echo "  Use --force to overwrite"
        exit 1
    else
        echo "${YELLOW}Overwriting existing installation at $BINARY_PATH${NC}"
    fi
fi

echo "${YELLOW}Fetching latest nightly release...${NC}"
RELEASE_JSON=$(curl -s "$GITHUB_API")

# Check if release exists
if echo "$RELEASE_JSON" | grep -q '"tag_name": "nightly"'; then
    echo "${GREEN}Found nightly release${NC}"
else
    echo "${RED}No nightly release found${NC}" >&2
    echo "Run the nightly build workflow manually on GitHub first:" >&2
    echo "  https://github.com/${REPO}/actions/workflows/nightly.yml" >&2
    exit 1
fi

ASSET_PATTERN="${TARGET}-[0-9a-f]\{7\}\.tar\.gz"
ASSET_URL=$(echo "$RELEASE_JSON" | jq -r ".assets[] | select(.name | test(\"${ASSET_PATTERN}\")) | .browser_download_url" | head -1)

if [ -z "$ASSET_URL" ]; then
    echo "${RED}No matching asset found for ${TARGET}-*${NC}" >&2
    echo "Available assets:" >&2
    echo "$RELEASE_JSON" | jq -r '.assets[].name' >&2
    exit 1
fi

ASSET_NAME=$(basename "$ASSET_URL")
echo "${YELLOW}Found asset: $ASSET_NAME${NC}"

TMP_DIR=$(mktemp -d)
TMP_FILE="$TMP_DIR/$ASSET_NAME"

echo "${YELLOW}Downloading...${NC}"
if [ "$DRY_RUN" = true ]; then
    echo "Would download: $ASSET_URL"
    echo "Would extract to: $INSTALL_DIR"
    echo "Binary would be installed at: $BINARY_PATH"
    rm -rf "$TMP_DIR"
    exit 0
fi

curl -sL "$ASSET_URL" -o "$TMP_FILE" || {
    echo "${RED}Failed to download: $ASSET_URL${NC}" >&2
    rm -rf "$TMP_DIR"
    exit 1
}

echo "${YELLOW}Extracting...${NC}"
if ! tar xzf "$TMP_FILE" -C "$TMP_DIR"; then
    echo "${RED}Failed to extract $ASSET_NAME${NC}" >&2
    rm -rf "$TMP_DIR"
    exit 1
fi

EXTRACTED_BINARY=$(find "$TMP_DIR" -type f -name "ryo" -not -name "*.tar.gz" | head -1)

if [ -z "$EXTRACTED_BINARY" ] || [ ! -f "$EXTRACTED_BINARY" ]; then
    echo "${RED}Binary 'ryo' not found in archive${NC}" >&2
    echo "Contents of archive:" >&2
    ls -la "$TMP_DIR" >&2
    rm -rf "$TMP_DIR"
    exit 1
fi

echo "${YELLOW}Installing to $BINARY_PATH${NC}"
mv "$EXTRACTED_BINARY" "$BINARY_PATH"
chmod +x "$BINARY_PATH"

rm -rf "$TMP_DIR"

echo ""
INSTALLED_VERSION=$($BINARY_PATH --version)
echo "${GREEN}Successfully installed Ryo!${NC}"
echo "  Version: $INSTALLED_VERSION"
echo "  Location: $BINARY_PATH"
echo ""
echo "${YELLOW}Next steps:${NC}"
echo "  1. Add to your PATH:"
echo "     export PATH=\"\$HOME/.ryo/bin:\\(PATH\""
echo ""
echo "  2. Test the installation:"
echo "     ryo --version"
echo "     ryo run examples/hello.ryo"
echo ""
echo "  3. Add the PATH line to your shell config (eller .bashrc, .zshrc, etc.)"
