#!/bin/bash
# Builds the Linux and Windows bundles on a remote Docker host, then copies them
# back to dist/linux-windows. The host only needs Docker; everything runs in a
# throwaway ubuntu:22.04 container (see container-build.sh) and is removed after.
#
# Usage: apps/whispering/scripts/release/build-linux-windows.sh user@host [cpus] [memory]
# Example: apps/whispering/scripts/release/build-linux-windows.sh root@build.example.com 6 8g
set -euo pipefail

HOST="${1:?usage: build-linux-windows.sh user@host [cpus] [memory]}"
CPUS="${2:-6}"
MEMORY="${3:-8g}"
REMOTE_DIR=/tmp/whispering-build
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
OUT_DIR="$REPO_ROOT/apps/whispering/dist/linux-windows"

ARCHIVE="$(mktemp -t whispering-src).tar.gz"
git -C "$REPO_ROOT" archive --format=tar.gz -o "$ARCHIVE" HEAD
echo "Packed $(git -C "$REPO_ROOT" rev-parse --short HEAD) (committed changes only)."

ssh "$HOST" "rm -rf $REMOTE_DIR && mkdir -p $REMOTE_DIR"
scp -q "$ARCHIVE" "$HOST:$REMOTE_DIR/whispering-src.tar.gz"
scp -q "$SCRIPT_DIR/container-build.sh" "$HOST:$REMOTE_DIR/whispering-remote-build.sh"
rm -f "$ARCHIVE"

ssh "$HOST" "chmod +x $REMOTE_DIR/whispering-remote-build.sh && \
	docker run --rm --name whispering-build --cpus=$CPUS --memory=$MEMORY \
	-v $REMOTE_DIR:/work ubuntu:22.04 bash -c '/work/whispering-remote-build.sh > /work/build.log 2>&1'" || true

mkdir -p "$OUT_DIR"
scp -q -r "$HOST:$REMOTE_DIR/out/*" "$OUT_DIR/" || true
scp -q "$HOST:$REMOTE_DIR/build.log" "$OUT_DIR/build.log" || true
ssh "$HOST" "rm -rf $REMOTE_DIR"

grep "ALL_DONE" "$OUT_DIR/build.log" || { echo "Build did not finish; see $OUT_DIR/build.log"; exit 1; }
find "$OUT_DIR" -type f ! -name build.log
