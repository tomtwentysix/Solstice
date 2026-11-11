#!/bin/sh
# Builds the SDK's sample plug-ins (delay, MDA synths and effects) used by the tests.
set -e
cd "$(dirname "$0")"
cmake -S . -B build-examples -DCMAKE_BUILD_TYPE=Release -DSOLSTICE_SDK_EXAMPLES=ON >/dev/null
cmake --build build-examples -j"$(nproc)" --target adelay mda-vst3
