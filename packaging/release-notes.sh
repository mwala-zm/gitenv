#!/bin/sh
# Print the release notes for one version: its section of CHANGELOG.md.
#
#   packaging/release-notes.sh <version> [changelog]
#
# <version> is the bare number without the leading v. The section is
# everything under the `## <version>` heading, which may go on with a date
# such as `## 0.4.0 (2026-11-07)`, up to the next `## ` heading, with the
# heading itself and the blank lines around it left out. Fails when the
# changelog has no such heading, so a release cannot go out without notes.
set -eu

version=$1
changelog=${2:-CHANGELOG.md}

if ! awk -v version="$version" '
  /^## / {
    if (found) exit
    if ($2 == version) { found = 1; next }
  }
  found && (started || NF) { started = 1; lines[++n] = $0 }
  END {
    if (!found) exit 1
    while (n > 0 && lines[n] == "") n--
    for (i = 1; i <= n; i++) print lines[i]
  }
' "$changelog"; then
  echo "release-notes.sh: $changelog has no '## $version' section" >&2
  exit 1
fi
