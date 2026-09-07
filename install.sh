#!/bin/sh
# Install the latest mtop release binary for this machine.
#   curl -fsSL https://raw.githubusercontent.com/Mariano215/mtop/main/install.sh | sh
# Downloads the archive and its SHA-256 from GitHub Releases, verifies the
# digest, and puts `mtop` in /usr/local/bin (if writable) or ~/.local/bin.
# Nothing else is touched. Set MTOP_VERSION=v0.1.1 to pin a version, or
# MTOP_BIN_DIR to choose the directory.
set -eu

repo="Mariano215/mtop"
os=$(uname -s)
arch=$(uname -m)
case "$os-$arch" in
  Linux-x86_64)             target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin-arm64)             target=aarch64-apple-darwin ;;
  Darwin-x86_64)            target=x86_64-apple-darwin ;;
  *) echo "mtop: no prebuilt binary for $os $arch; build with cargo instead" >&2; exit 1 ;;
esac

version="${MTOP_VERSION:-}"
if [ -z "$version" ]; then
  version=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
  [ -n "$version" ] || { echo "mtop: cannot read the latest release tag" >&2; exit 1; }
fi

name="mtop-$version-$target"
base="https://github.com/$repo/releases/download/$version"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "mtop: downloading $name.tar.gz"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz"
curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256"

cd "$tmp"
if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "$name.tar.gz.sha256" >/dev/null
else shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null; fi
echo "mtop: checksum verified"
tar -xzf "$name.tar.gz"

bin_dir="${MTOP_BIN_DIR:-}"
if [ -z "$bin_dir" ]; then
  if [ -w /usr/local/bin ]; then bin_dir=/usr/local/bin; else bin_dir="$HOME/.local/bin"; fi
fi
mkdir -p "$bin_dir"
install -m 755 "$name/mtop" "$bin_dir/mtop"
echo "mtop: installed $version to $bin_dir/mtop"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "mtop: add $bin_dir to your PATH, then run: mtop" ;;
esac
