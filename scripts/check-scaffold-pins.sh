#!/usr/bin/env bash
# Does every version `ream new` writes still reach what is published?
#
# A caret on 0.x is a ceiling, not a floor: `^0.1.27` stops resolving the moment
# the package publishes 0.2.0. So a pin table nobody rereads scaffolds apps
# against the line before the one being maintained, silently — which is what
# `@c9up/ream` and `@c9up/atlas` were doing when this check was written.
#
# Run from the ream-cli package root. Needs network; the publish workflow is the
# place for it, not `cargo test`.
set -euo pipefail

fail=0

# `("pkg", "^range"),` — the shape the tables in src/scaffold.rs use.
pins=$(grep -oE '\("(@c9up/[a-z-]+|reflect-metadata)", "\^[0-9][^"]*"\)' src/scaffold.rs)
if [ -z "$pins" ]; then
  echo "::error::found no pins in src/scaffold.rs — has the table moved?"
  exit 1
fi

while IFS= read -r pin; do
  pkg=$(printf '%s' "$pin" | sed -E 's/^\("([^"]+)".*/\1/')
  range=$(printf '%s' "$pin" | sed -E 's/.*"\^([^"]+)"\)$/\1/')
  [ "$pkg" = "reflect-metadata" ] && continue

  if ! latest=$(npm view --fetch-timeout=30000 "$pkg" version 2>/dev/null); then
    echo "::error::cannot reach the registry for $pkg — refusing to guess"
    fail=1
    continue
  fi

  # On 0.x a caret is bounded by the next minor, so the pin has to sit on the
  # published minor. On 1.x and up, by the next major.
  pin_major=${range%%.*}
  latest_major=${latest%%.*}
  pin_minor=$(printf '%s' "$range" | cut -d. -f2)
  latest_minor=$(printf '%s' "$latest" | cut -d. -f2)

  # A pin AHEAD of the registry resolves to nothing at all: `pnpm install`
  # fails outright on a version that was bumped locally and not published yet.
  # Checking only that the minor lines agree let `^0.2.9` past while the
  # registry held 0.2.8.
  highest=$(printf '%s\n%s\n' "$range" "$latest" | sort -V | tail -1)
  if [ "$highest" = "$range" ] && [ "$range" != "$latest" ]; then
    echo "::error::$pkg: scaffold pins ^$range, registry is only at $latest — nothing satisfies it"
    fail=1
  elif [ "$pin_major" != "$latest_major" ]; then
    echo "::error::$pkg: scaffold pins ^$range, registry is at $latest — a caret cannot cross a major"
    fail=1
  elif [ "$pin_major" = "0" ] && [ "$pin_minor" != "$latest_minor" ]; then
    echo "::error::$pkg: scaffold pins ^$range, registry is at $latest — on 0.x a caret cannot cross a minor"
    fail=1
  else
    echo "  ok  $pkg ^$range reaches $latest"
  fi
done <<< "$pins"

exit "$fail"
