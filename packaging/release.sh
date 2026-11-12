#!/bin/sh
# Usage: packaging/release.sh 0.1.1 "Fixes the plug-in window's tab name"
# Bumps the version everywhere it is recorded, commits, and tags v<version>.
set -e
cd "$(dirname "$0")/.."
version="$1"
notes="$2"
[ -n "$version" ] && [ -n "$notes" ] || { echo "usage: $0 <version> <notes>" >&2; exit 1; }
[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "commit your changes first" >&2; exit 1; }
sed -i "0,/^version = \".*\"/s//version = \"$version\"/" Cargo.toml
cargo update --workspace --offline -q
sed -i "s/%{!?solstice_version:[^}]*}/%{!?solstice_version:$version}/" packaging/solstice.spec
sed -i "/^%changelog/a * $(LC_ALL=C date '+%a %b %d %Y') SolarFlare Developments - $version-1\\n- $notes\\n" packaging/solstice.spec
sed -i "s|  <releases>|  <releases>\\n    <release version=\"$version\" date=\"$(date +%F)\"/>|" data/uk.co.solarflareit.Solstice.metainfo.xml
git commit -qam "Release $version

$notes"
git tag -a "v$version" -m "Solstice $version"
echo "Tagged v$version. Push with: git push && git push origin v$version"
