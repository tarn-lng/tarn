#!/bin/sh
set -eu
# Empty means the newest published release, resolved below.
VERSION=
PREFIX="${HOME}/.local/bin"
FORCE=0
UNINSTALL=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) VERSION=${2:?Missing version}; shift 2 ;;
    --prefix) PREFIX=${2:?Missing prefix}; shift 2 ;;
    --force) FORCE=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    --help) echo 'Usage: install.sh [--version X.Y.Z (default: newest release)] [--prefix DIRECTORY] [--force] [--uninstall]'; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done
case "$PREFIX" in /*) ;; *) echo 'Prefix must be an absolute directory' >&2; exit 1 ;; esac
MARKER="$PREFIX/.tarn-release"
if [ "$UNINSTALL" = 1 ]; then
  test -f "$MARKER" || { echo 'No managed Tarn installation found' >&2; exit 1; }
  (cd "$PREFIX"; sha256sum --check --status .tarn-release) || { echo 'Installed binaries changed; refusing uninstall' >&2; exit 1; }
  rm -f "$PREFIX/tarn" "$PREFIX/tarn-lsp" "$MARKER"
  echo 'Tarn uninstalled'; exit 0
fi
# Kept inline: the installer is a standalone release asset.
[ "$(uname -s)" = Linux ] || { echo 'Only Linux is supported' >&2; exit 1; }
case "$(uname -m)" in
  x86_64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) echo 'Only Linux x86_64 and ARM64 (aarch64) are supported' >&2; exit 1 ;;
esac
for tool in curl sha256sum tar mktemp; do command -v "$tool" >/dev/null || { echo "Missing tool: $tool" >&2; exit 1; }; done
if [ -z "$VERSION" ]; then
  # Every Tarn release is a prerelease, which GitHub's "latest" endpoint skips;
  # the release list is newest first. The archive is still checksum-verified below.
  VERSION=$(curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 \
    -H 'Accept: application/vnd.github+json' 'https://api.github.com/repos/tarn-lng/tarn/releases?per_page=1' |
    sed -n 's/.*"tag_name": *"v\([0-9][0-9.]*\)".*/\1/p' | head -n 1)
  [ -n "$VERSION" ] || { echo 'Cannot determine the newest Tarn release; pass --version X.Y.Z' >&2; exit 1; }
  echo "Installing Tarn $VERSION (newest release)"
fi
case "$VERSION" in ''|*[!0-9.]*|.*|*..*|*.) echo 'Use a numeric X.Y.Z release version' >&2; exit 1 ;; esac
mkdir -p "$PREFIX"
if [ "$FORCE" != 1 ] && { [ -e "$PREFIX/tarn" ] || [ -L "$PREFIX/tarn" ] || [ -e "$PREFIX/tarn-lsp" ] || [ -L "$PREFIX/tarn-lsp" ]; }; then
  test -f "$MARKER" && (cd "$PREFIX"; sha256sum --check --status .tarn-release) || { echo 'Existing binaries are not an unchanged managed installation; use --force to replace' >&2; exit 1; }
fi
TEMP=$(mktemp -d "$PREFIX/.tarn-install.XXXXXX")
trap 'rm -rf "$TEMP"' EXIT HUP INT TERM
ASSET="tarn-$VERSION-linux-$ARCH.tar.gz"
BASE="https://github.com/tarn-lng/tarn/releases/download/v$VERSION"
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 "$BASE/$ASSET" -o "$TEMP/$ASSET"
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 "$BASE/SHA256SUMS" -o "$TEMP/SHA256SUMS"
# The shared checksum file lists every architecture; download and check only
# the selected archive. Reject duplicate/missing entries before extraction.
awk -v asset="$ASSET" '$2 == asset {print; count++} END {if (count != 1) exit 1}' "$TEMP/SHA256SUMS" > "$TEMP/selected-sums"
(cd "$TEMP"; sha256sum --check --strict selected-sums)
# The trusted release contains exactly these regular files, with no directories or links.
CONTENTS=$(tar -tzf "$TEMP/$ASSET" | LC_ALL=C sort)
EXPECTED=$(printf 'COPYING\nREADME.md\ntarn\ntarn-lsp')
[ "$CONTENTS" = "$EXPECTED" ] || { echo 'Unexpected release archive contents' >&2; exit 1; }
tar -tvzf "$TEMP/$ASSET" | awk 'substr($1,1,1)!="-" {bad=1} END {exit bad}' || { echo 'Archive contains nonregular files' >&2; exit 1; }
mkdir "$TEMP/extracted"
tar -xzf "$TEMP/$ASSET" --no-same-owner --no-same-permissions -C "$TEMP/extracted"
chmod 755 "$TEMP/extracted/tarn" "$TEMP/extracted/tarn-lsp"
[ "$("$TEMP/extracted/tarn" version)" = "tarn $VERSION" ] || { echo 'Compiler version mismatch' >&2; exit 1; }
(cd "$TEMP/extracted"; sha256sum tarn tarn-lsp > .tarn-release)
mv -f "$TEMP/extracted/tarn" "$PREFIX/tarn"
mv -f "$TEMP/extracted/tarn-lsp" "$PREFIX/tarn-lsp"
mv -f "$TEMP/extracted/.tarn-release" "$MARKER"
echo "Installed Tarn $VERSION in $PREFIX"
echo "Add $PREFIX to PATH. Compiling native programs requires cc and libc development headers."
