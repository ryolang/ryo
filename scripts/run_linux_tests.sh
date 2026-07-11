#!/bin/bash
# Helper script to compile Ryo and execute its entire test suite (including
# ASan and LeakSanitizer smoke tests) in a native Linux Docker container.
set -e

# Change directory to the workspace root (this script lives in scripts/)
cd "$(dirname "$0")/.."

echo "========================================================"
echo "🐳 Building Ryo Linux test image..."
echo "========================================================"
docker build -t ryo-linux-test -f Dockerfile .

echo ""
echo "========================================================"
echo "🧪 Running Ryo tests under Linux (ASan & LSan active)..."
echo "========================================================"
# Check if stdin is a TTY to support both interactive developer shells and automated environments
if [ -t 0 ]; then
  TTY_FLAGS="-it"
else
  TTY_FLAGS=""
fi

docker run --rm $TTY_FLAGS \
  -v "$(pwd)":/usr/src/ryo \
  ryo-linux-test
