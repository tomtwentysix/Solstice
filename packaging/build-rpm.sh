#!/bin/sh
# Builds an RPM of the committed HEAD into dist/RPMS. Uncommitted changes are not included.
set -e
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
release="$(git rev-list --count HEAD).git$(git rev-parse --short HEAD)"
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
