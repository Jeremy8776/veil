#!/usr/bin/env bash
# Build, sign and (with Apple's credentials) notarise Veil for macOS, Apple Silicon and Intel:
#
#   $DIST/veil-<version>-macos-universal.dmg      Veil.app on a drag-to-Applications disk image
#   $DIST/veil-<version>-macos-universal.tar.gz   veil-node, veil-probe and the licences
#
# The approach follows ArtCraft's packaging/macos/package.sh.
#
# Inputs (env, all optional):
#   VEIL_VERSION          the version (default: veil-app's in Cargo.toml)
#   MACOS_SIGN_IDENTITY   codesign identity, set by import-cert.sh. Default "-": ad-hoc, for
#                         local testing; Gatekeeper then asks the person to allow Veil
#   MACOS_KEYCHAIN        the keychain holding that identity
#   APPLE_ID, APPLE_PASSWORD (app-specific), APPLE_TEAM_ID: notarisation. Missing: signed
#                         but not notarised, with a warning
#   DIST (default: dist), CARGO_TARGET_DIR (default: target)
#
# Usage: packaging/macos/package.sh [--skip-build]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
SKIP_BUILD=0
[ "${1:-}" = "--skip-build" ] && SKIP_BUILD=1

warn() {
  if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::warning::$*"; else echo "warning: $*" >&2; fi
}

VERSION="${VEIL_VERSION:-$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps --locked |
  python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "veil-app"))')}"
if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "error: '$VERSION' is not a version like 1.2.3" >&2
  exit 1
fi
DIST="${DIST:-$ROOT/dist}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
mkdir -p "$DIST"
DIST="$(cd "$DIST" && pwd)"
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
WORK="$CARGO_TARGET_DIR/macos-package"
APP="$WORK/Veil.app"
DMG="$DIST/veil-$VERSION-macos-universal.dmg"
TOOLS="veil-$VERSION-macos-universal"

NOTARISE=0
if [ "$IDENTITY" = "-" ]; then
  warn "macOS: ad-hoc signing (no MACOS_SIGN_IDENTITY); not notarised"
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  NOTARISE=1
else
  warn "macOS: APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID incomplete; signed but not notarised"
fi
echo "==> Veil $VERSION for macOS (universal), identity: $IDENTITY, notarise: $NOTARISE"

# ---- build -------------------------------------------------------------------------------------
if [ "$SKIP_BUILD" = 0 ]; then
  args=()
  for t in "${TARGETS[@]}"; do args+=(--target "$t"); done
  (cd "$ROOT" && cargo build --release --locked -p veil-app -p veil-node -p veil-probe "${args[@]}")
fi

rm -rf "$WORK"
mkdir -p "$WORK/bin"
for bin in veil-app veil-node veil-probe; do
  inputs=()
  for t in "${TARGETS[@]}"; do inputs+=("$CARGO_TARGET_DIR/$t/release/$bin"); done
  lipo -create -output "$WORK/bin/$bin" "${inputs[@]}"
  lipo -info "$WORK/bin/$bin"
done

# ---- signing helpers ---------------------------------------------------------------------------
sign() {
  # Hardened runtime and a secure timestamp for a real identity; ad-hoc cannot be timestamped.
  local ts=(--timestamp)
  [ "$IDENTITY" = "-" ] && ts=(--timestamp=none)
  local kc=()
  [ -n "${MACOS_KEYCHAIN:-}" ] && kc=(--keychain "$MACOS_KEYCHAIN")
  codesign --force --sign "$IDENTITY" ${kc[@]+"${kc[@]}"} "${ts[@]}" "$@"
}

notarise() {
  local file="$1" out id status
  echo "==> notarising $(basename "$file") (this can take a few minutes)"
  out="$(xcrun notarytool submit "$file" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
    --team-id "$APPLE_TEAM_ID" --wait --timeout 1h --output-format json)" || true
  echo "$out"
  id="$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null || true)"
  status="$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null || true)"
  if [ "$status" != "Accepted" ]; then
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" || true
    fi
    echo "error: notarisation of $(basename "$file") failed (status: ${status:-unknown})" >&2
    exit 1
  fi
}

# ---- Veil.app ----------------------------------------------------------------------------------
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
sed "s/@VERSION@/$VERSION/g" "$HERE/Info.plist.in" >"$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
cp "$WORK/bin/veil-app" "$APP/Contents/MacOS/veil-app"

# The icon, drawn from the app's 256 px PNG at each size the icon set needs up to 256.
ICONSET="$WORK/Veil.iconset"
mkdir -p "$ICONSET"
PNG="$ROOT/crates/veil-app/assets/veil-256.png"
for entry in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x 256:256x256; do
  size="${entry%%:*}"
  sips -z "$size" "$size" "$PNG" --out "$ICONSET/icon_${entry#*:}.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Veil.icns"

sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

if [ "$NOTARISE" = 1 ]; then
  ditto -c -k --keepParent "$APP" "$WORK/Veil-notarise.zip"
  notarise "$WORK/Veil-notarise.zip"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
fi

# ---- disk image --------------------------------------------------------------------------------
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/Veil.app"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "Veil" -srcfolder "$STAGE" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$DMG"
sign "$DMG"
codesign --verify --strict --verbose=2 "$DMG"
if [ "$NOTARISE" = 1 ]; then
  notarise "$DMG"
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
fi

# ---- tools -------------------------------------------------------------------------------------
# A bare executable cannot carry a stapled ticket, so Gatekeeper looks the notarisation up
# online the first time a downloaded copy runs.
TOOLS_DIR="$WORK/$TOOLS"
mkdir -p "$TOOLS_DIR"
for bin in veil-node veil-probe; do
  cp "$WORK/bin/$bin" "$TOOLS_DIR/$bin"
  sign --options runtime "$TOOLS_DIR/$bin"
  codesign --verify --strict --verbose=2 "$TOOLS_DIR/$bin"
done
for f in LICENSE-MIT LICENSE-APACHE README.md docs/DEPENDENCIES.md crates/veil-node/veil-node.example.toml; do
  cp "$ROOT/$f" "$TOOLS_DIR/"
done
if [ "$NOTARISE" = 1 ]; then
  ditto -c -k --keepParent "$TOOLS_DIR" "$WORK/tools-notarise.zip"
  notarise "$WORK/tools-notarise.zip"
fi
tar -czf "$DIST/$TOOLS.tar.gz" -C "$WORK" "$TOOLS"

ls -l "$DMG" "$DIST/$TOOLS.tar.gz"
