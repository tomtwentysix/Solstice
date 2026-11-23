#!/bin/sh
# Builds a Debian 12 (bookworm) package of the committed HEAD into dist/DEBS, in a container
# that carries the newer GTK stack Solstice needs. Uncommitted changes are not included.
set -e
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# As in build-rpm.sh: a tagged release builds as <version>-1, and commits after it sort after
# the release and before the next version.
tag="v$version"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null && [ "$(git rev-list --count "$tag..HEAD")" = 0 ]; then
    debversion="$version-1"
elif git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    debversion="$version+git$(git rev-list --count "$tag..HEAD").$(git rev-parse --short HEAD)-1"
else
    debversion="$version~git$(git rev-list --count HEAD).$(git rev-parse --short HEAD)-1"
fi
image=localhost/solstice-deb-bookworm
podman build -q -t "$image" packaging/deb >/dev/null

top="$PWD/dist"
mkdir -p "$top/SOURCES" "$top/DEBS"
tarball=$(packaging/make-tarball.sh "$top/SOURCES")
# Kept between builds so only changed crates recompile.
cache="${XDG_CACHE_HOME:-$HOME/.cache}/solstice-deb"
mkdir -p "$cache/target" "$cache/registry"
podman run --rm --security-opt label=disable \
    -v "$PWD/packaging/deb:/packaging:ro" -v "$top:/dist" \
    -v "$cache/target:/target" -v "$cache/registry:/root/.cargo/registry" -e CARGO_TARGET_DIR=/target \
    "$image" sh /packaging/package.sh "/dist/SOURCES/$(basename "$tarball")" "$debversion" /dist/DEBS "$(git config user.email)"
ls -1 "$top/DEBS/solstice_${debversion}_amd64.deb"
