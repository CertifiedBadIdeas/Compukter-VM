#!/usr/bin/env bash
# The Compukters Developers
# Copyright 2026 Vsevolod Petrov (lazyhat)
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

directory=${1:?bundle directory is required}
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' runtime-version.toml)
tag=${2:-v$version}
test "$tag" = "v$version"
commit=$(git rev-parse HEAD)
for suffix in linux-x86_64.tar.gz windows-x86_64.zip; do
    cargo run -p runtime-bundler --locked --offline -- inspect \
        "$directory/compukter-runtime-${version}-$suffix" \
        | jq -e --arg tag "$tag" --arg commit "$commit" \
            '.schema == 2 and .release_tag == $tag and .vm_commit == $commit and (.libraries | keys) == ["ffi", "jni"]'
done
(
    cd "$directory"
    sha256sum "compukter-runtime-${version}-linux-x86_64.tar.gz" \
        "compukter-runtime-${version}-windows-x86_64.zip" \
        > "compukter-runtime-${version}-checksums.sha256"
)
