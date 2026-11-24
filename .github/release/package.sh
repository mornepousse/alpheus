#!/bin/sh
# Package the three release binaries into alpheus-VERSION-x86_64-linux.tar.gz
# and write SHA256SUMS next to it.
# Usage: package.sh VERSION BIN_DIR OUT_DIR   (run from the repository root)
set -eu
version=${1:?usage: package.sh VERSION BIN_DIR OUT_DIR}
bindir=${2:?usage: package.sh VERSION BIN_DIR OUT_DIR}
out=${3:?usage: package.sh VERSION BIN_DIR OUT_DIR}
name=alpheus-$version-x86_64-linux

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name" "$out"
for b in alpheus alpheus-demon alpheus-launch; do
  install -m755 "$bindir/$b" "$stage/$name/$b"
  strip "$stage/$name/$b"
done
install -m644 LICENSE NOTICE README.md "$stage/$name/"
install -m644 .github/release/INSTALL "$stage/$name/INSTALL"

tar -C "$stage" --sort=name --owner=0 --group=0 --numeric-owner -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > SHA256SUMS)
