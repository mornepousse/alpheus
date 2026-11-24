#!/usr/bin/env bash
# Package the three release binaries into alpheus-VERSION-x86_64-linux.tar.gz
# and write SHA256SUMS next to it. The tarball is reproducible: sorted entries,
# numeric root owner, mtime from SOURCE_DATE_EPOCH (default: the HEAD commit
# time, else 0) and no gzip timestamp. The binaries are already stripped by the
# release profile, so they are not stripped again.
# Usage: package.sh VERSION BIN_DIR OUT_DIR   (run from the repository root)
set -euo pipefail
version=${1:?usage: package.sh VERSION BIN_DIR OUT_DIR}
bindir=${2:?usage: package.sh VERSION BIN_DIR OUT_DIR}
out=${3:?usage: package.sh VERSION BIN_DIR OUT_DIR}
name=alpheus-$version-x86_64-linux
epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name" "$out"
chmod 755 "$stage/$name"
for b in alpheus alpheus-demon alpheus-launch; do
  install -m755 "$bindir/$b" "$stage/$name/$b"
done
install -m644 LICENSE NOTICE README.md "$stage/$name/"
install -m644 .github/release/INSTALL "$stage/$name/INSTALL"

tar -C "$stage" --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
  -cf - "$name" | gzip -n > "$out/$name.tar.gz"
(cd "$out" && sha256sum "$name.tar.gz" > SHA256SUMS)
