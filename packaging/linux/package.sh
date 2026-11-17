#!/usr/bin/env bash
# Build and package Veil for Linux x86_64:
#
#   $DIST/veil-<version>-linux-x64.AppImage   any distribution with glibc >= the build host's;
#                                             updates itself (crates/veil-app/src/update/)
#   $DIST/veil-<version>-linux-x64.deb        Debian, Ubuntu, Mint, ...
#   $DIST/veil-<version>-linux-x64.rpm        Fedora, openSUSE, RHEL, ...
#   $DIST/veil-<version>-linux-x64.tar.gz     veil-app, veil-node, veil-probe and the licences
#
# Build on an old distribution (CI: Ubuntu 22.04, glibc 2.35) so the programs run on newer
# ones. The approach follows ArtCraft's packaging/linux/package.sh.
#
# Needs: cargo, nfpm (deb, rpm), appimagetool and the AppImage runtime. In CI these are
# downloaded at pinned versions and checked by SHA-256; locally set NFPM, APPIMAGETOOL and
# APPIMAGE_RUNTIME, or have nfpm and appimagetool on PATH (the runtime is then fetched).
#
# Usage: packaging/linux/package.sh [--skip-build]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
APP_ID=io.github.jeremy8776.veil
SKIP_BUILD=0
[ "${1:-}" = "--skip-build" ] && SKIP_BUILD=1

VERSION="${VEIL_VERSION:-$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps --locked |
  python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "veil-app"))')}"
if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "error: '$VERSION' is not a version like 1.2.3" >&2
  exit 1
fi
[ "$(uname -m)" = x86_64 ] || { echo "error: x86_64 only" >&2; exit 2; }
DIST="${DIST:-$ROOT/dist}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
mkdir -p "$DIST"
DIST="$(cd "$DIST" && pwd)"
BASENAME="veil-$VERSION-linux-x64"
echo "==> Veil $VERSION for Linux x64"

if [ "$SKIP_BUILD" = 0 ]; then
  (cd "$ROOT" && cargo build --release --locked -p veil-app -p veil-node -p veil-probe)
fi
BIN="$CARGO_TARGET_DIR/release"
WORK="$CARGO_TARGET_DIR/linux-package"
STAGE="$WORK/root"
rm -rf "$WORK"

# ---- one file tree for every format ------------------------------------------------------------
for bin in veil-app veil-node veil-probe; do
  install -Dm755 "$BIN/$bin" "$STAGE/usr/bin/$bin"
  strip "$STAGE/usr/bin/$bin"
done
install -Dm644 "$HERE/$APP_ID.desktop" "$STAGE/usr/share/applications/$APP_ID.desktop"
install -Dm644 "$ROOT/crates/veil-app/assets/veil-256.png" "$STAGE/usr/share/icons/hicolor/256x256/apps/$APP_ID.png"
for f in LICENSE-MIT LICENSE-APACHE README.md docs/DEPENDENCIES.md crates/veil-node/veil-node.example.toml; do
  install -Dm644 "$ROOT/$f" "$STAGE/usr/share/doc/veil/$(basename "$f")"
done
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop"
fi

# ---- deb and rpm -------------------------------------------------------------------------------
NFPM="${NFPM:-nfpm}"
export VERSION NFPM_ARCH=amd64
sed "s|\${STAGE}|$STAGE|g" "$HERE/nfpm.yaml" >"$WORK/nfpm.yaml"
for fmt in deb rpm; do
  (cd "$ROOT" && "$NFPM" package -f "$WORK/nfpm.yaml" -p "$fmt" -t "$DIST/$BASENAME.$fmt")
done

# ---- tar.gz ------------------------------------------------------------------------------------
TAR_DIR="$WORK/$BASENAME"
mkdir -p "$TAR_DIR"
cp "$STAGE/usr/bin/"* "$STAGE/usr/share/doc/veil/"* "$TAR_DIR/"
tar -czf "$DIST/$BASENAME.tar.gz" -C "$WORK" "$BASENAME"

# ---- AppImage ----------------------------------------------------------------------------------
APPDIR="$WORK/Veil.AppDir"
cp -a "$STAGE" "$APPDIR"
install -m755 "$HERE/AppRun" "$APPDIR/AppRun"
cp "$HERE/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
cp "$ROOT/crates/veil-app/assets/veil-256.png" "$APPDIR/$APP_ID.png"
TOOL="${APPIMAGETOOL:-appimagetool}"
runtime=()
[ -n "${APPIMAGE_RUNTIME:-}" ] && runtime=(--runtime-file "$APPIMAGE_RUNTIME")
# Extract-and-run: no FUSE needed on the build machine. The AppImage carries the static
# runtime, so users need no libfuse2 either.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream ${runtime[@]+"${runtime[@]}"} \
  "$APPDIR" "$DIST/$BASENAME.AppImage"

ls -l "$DIST/$BASENAME".*
