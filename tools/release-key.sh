#!/usr/bin/env bash
# Makes Veil's release key: the Ed25519 key the release workflow signs SHA256SUMS.txt with,
# and the app checks before it installs an update (crates/veil-app/src/update/verify.rs).
#
# - The private key goes to a file outside the repository (default
#   ~/.veil/release-key.pem, readable only by you). It is never printed.
# - Its public half is written to crates/veil-app/release-key.pub, to commit.
# - Then store the private key as the `VEIL_UPDATE_KEY` secret of the `release` environment:
#     gh secret set VEIL_UPDATE_KEY --env release --repo Jeremy8776/veil < ~/.veil/release-key.pem
#   and keep an offline copy. Losing it means shipping a new public key, which copies already
#   installed accept only through a manual install.
#
# Refuses to overwrite an existing key. Needs OpenSSL 3 (Git for Windows ships it).
#
# Usage: tools/release-key.sh [path-for-the-private-key]
set -euo pipefail
umask 077

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KEY="${1:-$HOME/.veil/release-key.pem}"
PUB="$ROOT/crates/veil-app/release-key.pub"

if [ -e "$KEY" ]; then
  echo "error: $KEY already exists; not replacing a release key" >&2
  exit 1
fi
mkdir -p "$(dirname "$KEY")"
openssl genpkey -algorithm ed25519 -out "$KEY"
# The raw 32-byte public key is the last 32 bytes of its DER form.
openssl pkey -in "$KEY" -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n' >"$PUB"
printf '\n' >>"$PUB"
echo "private key: $KEY (keep it secret, keep an offline copy)"
echo "public key:  $(cat "$PUB") written to crates/veil-app/release-key.pub"
echo "next: gh secret set VEIL_UPDATE_KEY --env release --repo Jeremy8776/veil < \"$KEY\""
