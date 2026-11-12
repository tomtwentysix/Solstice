#!/bin/sh
# Builds an RPM of the committed HEAD into dist/RPMS. Uncommitted changes are not included.
set -e
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# A tagged release (v0.1.0) builds as release 1. Commits after it build as 1.<commits since
# the tag>.git<hash>, which sort after the release and before the next version.
tag="v$version"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null && [ "$(git rev-list --count "$tag..HEAD")" = 0 ]; then
    release=1
elif git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    release="1.$(git rev-list --count "$tag..HEAD").git$(git rev-parse --short HEAD)"
else
    release="0.$(git rev-list --count HEAD).git$(git rev-parse --short HEAD)"
fi
top="$PWD/dist"
mkdir -p "$top/SOURCES"
packaging/make-tarball.sh "$top/SOURCES" >/dev/null
# Kept between builds so only changed crates recompile.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/solstice-rpm/target}"
rpmbuild -bb packaging/solstice.spec \
    --define "_topdir $top" \
    --define "solstice_version $version" \
    --define "solstice_release $release"
ls -1 "$top"/RPMS/*/solstice-"$version-$release"*.rpm
