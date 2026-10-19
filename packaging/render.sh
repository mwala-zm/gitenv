#!/bin/sh
# Render the Homebrew formula and the AUR PKGBUILD for one release.
#
#   packaging/render.sh <version> <assets-dir> <out-dir>
#
# <version> is the bare number without the leading v. <assets-dir> holds the
# four release archives named gitenv-<target>.tar.gz, exactly as the release
# workflow uploads them; their checksums are what the templates need. Writes
# <out-dir>/gitenv.rb and <out-dir>/PKGBUILD.
set -eu

version=$1
assets=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)

sha() {
  file="$assets/gitenv-$1.tar.gz"
  if [ ! -f "$file" ]; then
    echo "render.sh: missing $file" >&2
    exit 1
  fi
  shasum -a 256 "$file" | cut -d' ' -f1
}

sha_macos_arm=$(sha aarch64-apple-darwin)
sha_macos_intel=$(sha x86_64-apple-darwin)
sha_linux_arm=$(sha aarch64-unknown-linux-gnu)
sha_linux_intel=$(sha x86_64-unknown-linux-gnu)

render() {
  sed \
    -e "s|@VERSION@|$version|g" \
    -e "s|@SHA_MACOS_ARM@|$sha_macos_arm|g" \
    -e "s|@SHA_MACOS_INTEL@|$sha_macos_intel|g" \
    -e "s|@SHA_LINUX_ARM@|$sha_linux_arm|g" \
    -e "s|@SHA_LINUX_INTEL@|$sha_linux_intel|g" \
    "$1" > "$2"
  if grep -q '@[A-Z_]*@' "$2"; then
    echo "render.sh: unfilled placeholder in $2" >&2
    exit 1
  fi
}

mkdir -p "$out"
render "$here/homebrew/gitenv.rb.in" "$out/gitenv.rb"
render "$here/aur/PKGBUILD.in" "$out/PKGBUILD"
echo "rendered $out/gitenv.rb and $out/PKGBUILD for $version"
