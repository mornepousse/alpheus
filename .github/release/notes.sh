#!/usr/bin/env bash
# Print the CHANGELOG.md section of one version (without its "## [x.y.z]" line)
# to standard output. Fails when the section is missing or empty. Relative link
# targets are rewritten to the repository's files at the tag vVERSION, since a
# relative link 404s in a release body; http(s), mailto and #anchor links stay.
# Usage: notes.sh VERSION [CHANGELOG]
set -euo pipefail
version=${1:?usage: notes.sh VERSION [CHANGELOG]}
changelog=${2:-CHANGELOG.md}
base=https://github.com/mornepousse/alpheus/blob/v$version
notes=$(awk -v v="$version" '
  /^## / { if (on) exit; on = (index($0, "## [" v "]") == 1); next }
  on { print }
' "$changelog")
if [ -z "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
  echo "no CHANGELOG section for $version" >&2
  exit 1
fi
# A target is relative when it starts with a path character and has no scheme (no colon).
printf '%s\n' "$notes" | sed -E "s#\]\(([A-Za-z0-9_.][^):]*)\)#](${base}/\1)#g"
