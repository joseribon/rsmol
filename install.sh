#!/bin/sh
# Installs the rsmol desktop app from the GitHub releases, on macOS and Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/joseribon/rsmol/main/install.sh | sh
#
# Options (after `sh -s --` when piped):
#   --version X.Y.Z   install that release instead of the latest (or set RSMOL_VERSION)
#   --prefix DIR      Linux: install under DIR instead of ~/.local (or set RSMOL_PREFIX)
#   --uninstall       remove what a previous run installed
#
# Environment: RSMOL_APPDIR (macOS: where rsmol.app goes), RSMOL_DOWNLOAD_BASE (a mirror, or a
# local folder as file:///path, holding the release assets and SHA256SUMS).
#
# macOS gets rsmol.app in /Applications (or ~/Applications when that is not writable);
# Linux gets bin/, share/applications, icons and the bundled Python package under the prefix.
# Either way an `rsmol` command is linked into ~/.local/bin (or <prefix>/bin).
set -eu

REPO=joseribon/rsmol
version=${RSMOL_VERSION:-latest}
prefix=${RSMOL_PREFIX:-$HOME/.local}
uninstall=0

usage() {
  cat <<'EOF_USAGE'
usage: install.sh [--version X.Y.Z] [--prefix DIR] [--uninstall]
  --version X.Y.Z   install that release instead of the latest (or set RSMOL_VERSION)
  --prefix DIR      Linux: install under DIR instead of ~/.local (or set RSMOL_PREFIX)
  --uninstall       remove what a previous run installed
EOF_USAGE
}

say() { printf '%s\n' "rsmol: $*"; }
die() { printf '%s\n' "rsmol: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "this installer needs '$1'"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; version=${2#v}; shift 2 ;;
    --prefix) [ $# -ge 2 ] || die "--prefix needs a value"; prefix=$2; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

os=$(uname -s)
case "$os" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) die "unsupported system: $os (rsmol releases cover macOS and Linux; on Windows, build from source)" ;;
esac
bindir="$prefix/bin"
manifest="$prefix/share/rsmol/install-manifest"

# --- uninstall ------------------------------------------------------------------------------
if [ "$uninstall" = 1 ]; then
  if [ "$os" = macos ]; then
    for app in ${RSMOL_APPDIR:+"$RSMOL_APPDIR/rsmol.app"} /Applications/rsmol.app "$HOME/Applications/rsmol.app"; do
      [ -d "$app" ] && rm -rf "$app" && say "removed $app"
    done
    [ -L "$bindir/rsmol" ] && rm -f "$bindir/rsmol" && say "removed $bindir/rsmol"
  else
    [ -f "$manifest" ] || die "nothing to uninstall under $prefix (no $manifest)"
    while IFS= read -r f; do rm -f "$prefix/$f"; done < "$manifest"
    rm -f "$manifest"
    rmdir "$prefix/share/rsmol/python/rsmol" "$prefix/share/rsmol/python" "$prefix/share/rsmol" "$prefix/share/doc/rsmol" 2>/dev/null || true
    say "removed rsmol from $prefix"
  fi
  exit 0
fi

# --- download -------------------------------------------------------------------------------
need curl
need uname
if [ "$os" = macos ]; then
  asset=rsmol-macos-universal.zip
  need ditto
else
  arch=$(uname -m)
  case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "no Linux build for $arch yet (x86_64 and aarch64 only); see the README to build from source" ;;
  esac
  asset=rsmol-linux-$arch.tar.gz
  need tar
fi

proto='=https'
if [ -n "${RSMOL_DOWNLOAD_BASE:-}" ]; then
  base=${RSMOL_DOWNLOAD_BASE%/}
  proto='=https,file'
elif [ "$version" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/v$version"
fi

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t rsmol)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset ($version)"
curl -fL --proto "$proto" --tlsv1.2 --progress-bar -o "$tmp/$asset" "$base/$asset" \
  || die "download failed: $base/$asset"
curl -fsSL --proto "$proto" --tlsv1.2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" \
  || die "could not download the checksums: $base/SHA256SUMS"

expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "$asset is not listed in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$asset" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')
else
  die "need sha256sum or shasum to verify the download"
fi
[ "$actual" = "$expected" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"

mkdir -p "$bindir"

# --- install: macOS -------------------------------------------------------------------------
if [ "$os" = macos ]; then
  ditto -x -k "$tmp/$asset" "$tmp/x"
  if [ -n "${RSMOL_APPDIR:-}" ]; then appdir=$RSMOL_APPDIR
  elif [ -w /Applications ]; then appdir=/Applications
  else appdir="$HOME/Applications"; fi
  mkdir -p "$appdir"
  rm -rf "$appdir/rsmol.app"
  mv "$tmp/x/rsmol.app" "$appdir/rsmol.app"
  # curl does not quarantine downloads; this only matters if the archive came another way.
  xattr -dr com.apple.quarantine "$appdir/rsmol.app" 2>/dev/null || true
  ln -sf "$appdir/rsmol.app/Contents/MacOS/rsmol-desktop" "$bindir/rsmol"
  say "installed $appdir/rsmol.app"
  say "open it from Launchpad or Spotlight, or run 'rsmol [file]'"

# --- install: Linux -------------------------------------------------------------------------
else
  tar -xzf "$tmp/$asset" -C "$tmp"
  src="$tmp/rsmol-linux-$arch"
  [ -x "$src/bin/rsmol-desktop" ] || die "unexpected archive layout"
  # Replace a previous install's files first, so files dropped from the release do not linger.
  if [ -f "$manifest" ]; then
    while IFS= read -r f; do rm -f "$prefix/$f"; done < "$manifest"
  fi
  (cd "$src" && find . -type f | sed 's|^\./||') > "$tmp/manifest"
  while IFS= read -r f; do
    mkdir -p "$prefix/$(dirname "$f")"
    cp "$src/$f" "$prefix/$f"
  done < "$tmp/manifest"
  ln -sf rsmol-desktop "$bindir/rsmol"
  echo bin/rsmol >> "$tmp/manifest"
  mkdir -p "$(dirname "$manifest")"
  cp "$tmp/manifest" "$manifest"
  echo share/rsmol/install-manifest >> "$manifest"

  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$prefix/share/applications" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$prefix/share/icons/hicolor" 2>/dev/null || true

  # The binary links the desktop's own GTK 3, fontconfig and xkbcommon.
  if command -v ldd >/dev/null 2>&1; then
    missing=$(ldd "$prefix/bin/rsmol-desktop" 2>/dev/null | awk '/not found/ { print $1 }' | tr '\n' ' ')
    [ -z "$missing" ] || say "warning: missing system libraries: $missing(install gtk3, fontconfig and libxkbcommon from your package manager)"
  fi
  say "installed rsmol under $prefix"
  say "find it in your applications menu, or run 'rsmol [file]'"
fi

case ":$PATH:" in
  *":$bindir:"*) ;;
  *) say "note: $bindir is not on your PATH; add it in your shell profile to use the 'rsmol' command" ;;
esac
