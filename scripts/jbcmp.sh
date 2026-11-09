#!/usr/bin/env bash
# Compare our formatter with ReSharper CLI's "Reformat Code" profile.
# Usage: scripts/jbcmp.sh <source.cs>
# Requires jbtmp/t.sln to exist (see README notes).
# On success the ReSharper output is stored as fixtures/expected/<name>.
set -u
src="$1"
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$root/jbtmp"
name="$(basename "$src")"
mkdir -p "$root/fixtures/expected"
cp "$src" "$tmp/A.cs"
( cd "$tmp" && dotnet jb cleanupcode t.sln --no-build --profile="Built-in: Reformat Code" >/dev/null 2>&1 )
cp "$tmp/A.cs" "$root/fixtures/expected/$name"
cargo run -q -p csformat-cli --manifest-path "$root/Cargo.toml" -- stdin --file-name "$src" < "$src" > "$tmp/ours.cs" 2>/dev/null
if diff -u "$root/fixtures/expected/$name" "$tmp/ours.cs"; then
  echo "SAME: $src"
else
  echo "DIFF: $src"
  exit 1
fi
