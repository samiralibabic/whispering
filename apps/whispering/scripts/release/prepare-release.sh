#!/bin/bash
# Collects the built bundles into dist/release-vX.Y.Z, signs the Linux and
# Windows updater artifacts, and writes latest.json for the Tauri updater.
#
# Expects:
#   - macOS builds in src-tauri/target/{aarch64,x86_64}-apple-darwin/release/bundle
#     (built with TAURI_SIGNING_PRIVATE_KEY set, so their .app.tar.gz.sig exists)
#   - Linux and Windows builds in dist/linux-windows (from build-linux-windows.sh)
#   - the updater private key at $WHISPERING_UPDATER_KEY (default ~/.tauri/whispering-fork.key)
#
# Usage: apps/whispering/scripts/release/prepare-release.sh "Release notes for latest.json"
set -euo pipefail

NOTES="${1:-}"
APP_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
KEY="${WHISPERING_UPDATER_KEY:-$HOME/.tauri/whispering-fork.key}"
VERSION="$(jq -r .version "$APP_DIR/src-tauri/tauri.conf.json")"
REPO="$(grep -o "WHISPERING_GITHUB_REPO = '[^']*'" "$APP_DIR/src/lib/constants/app/urls.ts" | cut -d"'" -f2)"
BASE_URL="https://github.com/$REPO/releases/download/v$VERSION"
OUT="$APP_DIR/dist/release-v$VERSION"
LW="$APP_DIR/dist/linux-windows"

rm -rf "$OUT" && mkdir -p "$OUT"
platforms='{}'

add_platform() { # key file signature
	platforms="$(jq --arg k "$1" --arg u "$BASE_URL/$2" --arg s "$3" '. + {($k): {signature: $s, url: $u}}' <<<"$platforms")"
}

sign() { # file -> creates file.sig
	# bun drops empty arguments, so pass the empty password as --password=
	(cd "$APP_DIR" && bun tauri signer sign --private-key-path "$KEY" --password= "$1" >/dev/null)
}

for target in aarch64:aarch64 x86_64:x64; do
	rust_arch="${target%%:*}"
	label="${target##*:}"
	bundle="$APP_DIR/src-tauri/target/$rust_arch-apple-darwin/release/bundle"
	[ -f "$bundle/macos/Whispering.app.tar.gz.sig" ] || { echo "skip macOS $rust_arch (no signed build)"; continue; }
	cp "$bundle/dmg/Whispering_${VERSION}_${label}.dmg" "$OUT/"
	cp "$bundle/macos/Whispering.app.tar.gz" "$OUT/Whispering_${VERSION}_${label}.app.tar.gz"
	cp "$bundle/macos/Whispering.app.tar.gz.sig" "$OUT/Whispering_${VERSION}_${label}.app.tar.gz.sig"
	add_platform "darwin-$rust_arch" "Whispering_${VERSION}_${label}.app.tar.gz" "$(cat "$bundle/macos/Whispering.app.tar.gz.sig")"
done

appimage="$(find "$LW" -name "*_${VERSION}_*.AppImage" 2>/dev/null | head -1)"
if [ -n "$appimage" ]; then
	cp "$appimage" "$LW"/linux/*.deb "$LW"/linux/*.rpm "$OUT/" 2>/dev/null || true
	name="$(basename "$appimage")"
	sign "$OUT/$name"
	add_platform "linux-x86_64" "$name" "$(cat "$OUT/$name.sig")"
else
	echo "skip Linux (no AppImage in $LW)"
fi

setup="$(find "$LW" -name "*_${VERSION}_*-setup.exe" 2>/dev/null | head -1)"
if [ -n "$setup" ]; then
	cp "$setup" "$OUT/"
	name="$(basename "$setup")"
	sign "$OUT/$name"
	add_platform "windows-x86_64" "$name" "$(cat "$OUT/$name.sig")"
else
	echo "skip Windows (no setup .exe in $LW)"
fi

jq -n --arg v "$VERSION" --arg n "$NOTES" --arg d "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson p "$platforms" \
	'{version: $v, notes: $n, pub_date: $d, platforms: $p}' >"$OUT/latest.json"

echo "Release files in $OUT:"
ls -1 "$OUT"
jq '.platforms | keys' "$OUT/latest.json"

# Every updater artifact must verify against the public key the app ships with.
signed=()
while IFS= read -r sig_file; do signed+=("${sig_file%.sig}"); done < <(find "$OUT" -name "*.sig")
bun "$(dirname "$0")/verify-signatures.ts" "$APP_DIR/src-tauri/tauri.conf.json" "${signed[@]}"
