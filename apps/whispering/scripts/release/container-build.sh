#!/bin/bash
# Builds Whispering for Linux (AppImage, deb, rpm) and Windows (NSIS installer,
# cross-compiled with cargo-xwin) inside a throwaway ubuntu:22.04 container.
# Started by build-linux-windows.sh; expects the source at /work/whispering-src.tar.gz
# and writes bundles to /work/out. Updater artifacts are signed later, on the
# maintainer's Mac, by prepare-release.sh.
set -uo pipefail
export DEBIAN_FRONTEND=noninteractive
export APPIMAGE_EXTRACT_AND_RUN=1
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

step() { echo; echo "=== $* ($(date -u +%H:%M:%S))"; }

step "apt dependencies"
apt-get update -qq
apt-get install -y -qq curl wget file unzip xz-utils git ca-certificates gnupg xdg-utils \
	build-essential pkg-config cmake clang lld llvm nsis libssl-dev \
	libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libxdo-dev \
	libasound2-dev libopenblas-dev libx11-dev libxtst-dev libxrandr-dev \
	libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
	gstreamer1.0-plugins-base gstreamer1.0-plugins-good >/dev/null || exit 10
# Ubuntu ships clang-cl only as a versioned binary; clang switches to MSVC mode by name.
[ -e /usr/local/bin/clang-cl ] || ln -sf "$(command -v clang)" /usr/local/bin/clang-cl

step "Vulkan SDK (whisper.cpp on Linux uses Vulkan)"
wget -qO- https://packages.lunarg.com/lunarg-signing-key-pub.asc > /etc/apt/trusted.gpg.d/lunarg.asc
wget -qO /etc/apt/sources.list.d/lunarg-vulkan-1.3.290-jammy.list https://packages.lunarg.com/vulkan/1.3.290/lunarg-vulkan-1.3.290-jammy.list
apt-get update -qq && apt-get install -y -qq vulkan-sdk >/dev/null || exit 11

step "bun, rust, cargo-xwin"
curl -fsSL https://bun.sh/install | bash -s "bun-v1.3.14" >/dev/null || exit 12
curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null || exit 13
export PATH="$HOME/.bun/bin:$HOME/.cargo/bin:$PATH"
rustup target add x86_64-pc-windows-msvc
cargo install --locked cargo-xwin >/dev/null 2>&1 || exit 14

step "source"
mkdir -p /src /work/out/linux /work/out/windows
tar -xzf /work/whispering-src.tar.gz -C /src || exit 15
cd /src && bun install || exit 16
cd /src/apps/whispering

step "Linux build"
bun tauri build --bundles appimage,deb,rpm --config '{"bundle":{"createUpdaterArtifacts":false}}'
LINUX_EXIT=$?
echo "LINUX_EXIT=$LINUX_EXIT"
cp src-tauri/target/release/bundle/appimage/*.AppImage src-tauri/target/release/bundle/deb/*.deb \
	src-tauri/target/release/bundle/rpm/*.rpm /work/out/linux/ 2>/dev/null

step "Windows SDK (xwin)"
(cd src-tauri && cargo xwin env --target x86_64-pc-windows-msvc >/dev/null) || exit 17
# The splatted SDK has only lowercase and uppercase file names; ort links these
# with mixed case, which a case-sensitive file system does not resolve.
SDK_LIB="$HOME/.cache/cargo-xwin/xwin/sdk/lib/um/x86_64"
ln -sf directml.lib "$SDK_LIB/DirectML.lib"
ln -sf pathcch.lib "$SDK_LIB/PathCch.lib"

step "Windows build"
# Cross builds produce the NSIS installer only; --bundles nsis is rejected on a Linux host.
bun tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --config '{"bundle":{"createUpdaterArtifacts":false}}'
WINDOWS_EXIT=$?
echo "WINDOWS_EXIT=$WINDOWS_EXIT"
cp src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe /work/out/windows/ 2>/dev/null

step "done"
ls -la /work/out/linux /work/out/windows
echo "ALL_DONE LINUX_EXIT=$LINUX_EXIT WINDOWS_EXIT=$WINDOWS_EXIT"
