#!/bin/sh
# Print the CHANGELOG.md section of one version (without its "## [x.y.z]" line)
# to standard output. Fails when the section is missing or empty.
# Usage: notes.sh VERSION [CHANGELOG]
set -eu
version=${1:?usage: notes.sh VERSION [CHANGELOG]}
changelog=${2:-CHANGELOG.md}
notes=$(awk -v v="$version" '
  /^## / { if (on) exit; on = (index($0, "## [" v "]") == 1); next }
  on { print }
' "$changelog")
if [ -z "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
  echo "no CHANGELOG section for $version" >&2
  exit 1
fi
printf '%s\n' "$notes"
