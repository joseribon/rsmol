#!/usr/bin/env bash
# Lays out a release archive from a built rsmol-desktop binary. Used by the release workflow;
# runs locally too:
#
#   cargo build --release -p rsmol-desktop
#   packaging/package.sh macos universal target/release/rsmol-desktop dist   # rsmol-macos-universal.zip
#   packaging/package.sh linux x86_64    target/release/rsmol-desktop dist   # rsmol-linux-x86_64.tar.gz
#
# macOS: rsmol.app with the icon and the pure-Python `rsmol` package in Contents/Resources.
#   Signed ad hoc unless MACOS_SIGN_IDENTITY names a Developer ID certificate in the keychain.
# Linux: a prefix tree (bin/, share/...) that install.sh, Homebrew and packagers copy as is.
#
# Asset names carry no version so that releases/latest/download/<name> always resolves.
set -euo pipefail

if [ $# -ne 4 ]; then
  echo "usage: $0 <macos|linux> <arch> <path/to/rsmol-desktop> <out-dir>" >&2
  exit 2
fi
os=$1 arch=$2 binary=$3 out=$4
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
[ -n "$version" ] || { echo "could not read the version from Cargo.toml" >&2; exit 1; }
[ -x "$binary" ] || { echo "not an executable: $binary" >&2; exit 1; }
mkdir -p "$out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# The pure-Python package only: the app's scripts talk to it over a socket, so a compiled
# _native module (left by local maturin builds) and bytecode caches stay out.
copy_python() {
  mkdir -p "$1/rsmol"
  cp "$root"/crates/rsmol-py/python/rsmol/*.py "$root/crates/rsmol-py/python/rsmol/py.typed" "$1/rsmol/"
}

# install -D without GNU install (macOS has the BSD one): put <mode> <src> <dest>
put() {
  mkdir -p "$(dirname "$3")"
  cp "$2" "$3"
  chmod "$1" "$3"
}

case "$os" in
  macos)
    app="$stage/rsmol.app/Contents"
    mkdir -p "$app/MacOS" "$app/Resources"
    cp "$binary" "$app/MacOS/rsmol-desktop"
    sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Info.plist"
    cp "$root/packaging/icons/rsmol.icns" "$app/Resources/rsmol.icns"
    copy_python "$app/Resources/python"
    if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
      codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$stage/rsmol.app"
    else
      codesign --force --sign - "$stage/rsmol.app"
    fi
    codesign --verify --strict "$stage/rsmol.app"
    asset="rsmol-macos-$arch.zip"
    rm -f "$out/$asset"
    ditto -c -k --keepParent "$stage/rsmol.app" "$out/$asset"
    ;;
  linux)
    name="rsmol-linux-$arch"
    pfx="$stage/$name"
    id=io.github.joseribon.rsmol
    put 755 "$binary" "$pfx/bin/rsmol-desktop"
    copy_python "$pfx/share/rsmol/python"
    put 644 "$root/packaging/linux/$id.desktop" "$pfx/share/applications/$id.desktop"
    put 644 "$root/packaging/linux/$id.metainfo.xml" "$pfx/share/metainfo/$id.metainfo.xml"
    (cd "$root/packaging/icons" && find hicolor -name "$id.png") | while read -r icon; do
      put 644 "$root/packaging/icons/$icon" "$pfx/share/icons/$icon"
    done
    put 644 "$root/LICENSE-MIT" "$pfx/share/doc/rsmol/LICENSE-MIT"
    put 644 "$root/LICENSE-APACHE" "$pfx/share/doc/rsmol/LICENSE-APACHE"
    put 644 "$root/README.md" "$pfx/share/doc/rsmol/README.md"
    asset="$name.tar.gz"
    tar -C "$stage" -czf "$out/$asset" "$name"
    ;;
  *)
    echo "unknown OS '$os' (expected macos or linux)" >&2
    exit 2
    ;;
esac

echo "rsmol $version: $out/$asset"
