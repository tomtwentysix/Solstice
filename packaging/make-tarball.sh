#!/bin/sh
# git archive leaves out submodules, so the VST3 SDK is appended separately.
set -e
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
prefix="solstice-$version"
out="$PWD/packaging/$prefix.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git archive --prefix="$prefix/" -o "$tmp/main.tar" HEAD
(cd vst3shim/vst3sdk && git archive --prefix="$prefix/vst3shim/vst3sdk/" -o "$tmp/sdk.tar" HEAD)
for sub in base cmake pluginterfaces public.sdk; do
    (cd "vst3shim/vst3sdk/$sub" && git archive --prefix="$prefix/vst3shim/vst3sdk/$sub/" -o "$tmp/$sub.tar" HEAD)
done
for t in "$tmp"/sdk.tar "$tmp"/base.tar "$tmp"/cmake.tar "$tmp"/pluginterfaces.tar "$tmp"/public.sdk.tar; do
    tar --concatenate -f "$tmp/main.tar" "$t"
done
gzip -c "$tmp/main.tar" > "$out"
echo "$out"
