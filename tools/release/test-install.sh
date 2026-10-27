#!/bin/sh
set -eu
ROOT=$(pwd)
ARCH=$(sh tools/release/platform.sh)
TEMP=$(mktemp -d)
trap 'rm -rf "$TEMP"' EXIT HUP INT TERM
VERSION=$(target/release/tarn version | cut -d ' ' -f 2)
tools/release/package.sh "$VERSION"
# A release contains checksums for both architectures; the other archive is
# deliberately absent from this fixture to exercise selective verification.
printf '%s  tarn-%s-linux-other.tar.gz\n' "$(printf '%064d' 0)" "$VERSION" >> target/dist/SHA256SUMS
mkdir "$TEMP/mock"
cat > "$TEMP/mock/curl" <<'MOCK'
#!/bin/sh
set -eu
for arg in "$@"; do
  case "$arg" in https://api.github.com/repos/tarn-lng/tarn/releases*) printf '[\n  {\n    "tag_name": "v%s",\n' "$FIXTURE_VERSION"; exit 0 ;; esac
  case "$arg" in https://github.com/tarn-lng/tarn/releases/download/*) SOURCE=${arg##*/} ;; esac
  if [ "${PREVIOUS:-}" = -o ]; then DEST=$arg; fi
  PREVIOUS=$arg
done
cp "$FIXTURE/$SOURCE" "$DEST"
MOCK
chmod +x "$TEMP/mock/curl"
export FIXTURE="$ROOT/target/dist"
export FIXTURE_VERSION="$VERSION"
export PATH="$TEMP/mock:$PATH"
tools/release/install.sh --version "$VERSION" --prefix "$TEMP/bin"
"$TEMP/bin/tarn" init "$TEMP/project"
(cd "$TEMP/project"; "$TEMP/bin/tarn" run)
tools/release/install.sh --version "$VERSION" --prefix "$TEMP/bin"
tools/release/install.sh --prefix "$TEMP/bin" --uninstall
test ! -e "$TEMP/bin/tarn"
# Without --version the installer resolves the newest release.
tools/release/install.sh --prefix "$TEMP/bin" > "$TEMP/latest.log"
grep -q "Installing Tarn $VERSION (newest release)" "$TEMP/latest.log"
"$TEMP/bin/tarn" version | grep -qx "tarn $VERSION"
tools/release/install.sh --prefix "$TEMP/bin" --uninstall
printf 'unmanaged' > "$TEMP/bin/tarn"
if tools/release/install.sh --version "$VERSION" --prefix "$TEMP/bin"; then echo 'Unmanaged binary overwritten' >&2; exit 1; fi
rm "$TEMP/bin/tarn"
cp target/dist/SHA256SUMS "$TEMP/sums"
printf 'corruption' >> "target/dist/tarn-$VERSION-linux-$ARCH.tar.gz"
if tools/release/install.sh --version "$VERSION" --prefix "$TEMP/bin"; then echo 'Corrupt download accepted' >&2; exit 1; fi
test ! -e "$TEMP/bin/tarn"
tools/release/package.sh "$VERSION"
echo 'PASS: installer, newest-release default, update, uninstall, existing-file protection and corruption rejection'
