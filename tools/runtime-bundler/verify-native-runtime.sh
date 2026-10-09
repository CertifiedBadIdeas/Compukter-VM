#!/usr/bin/env bash
# The Compukters Developers
# Copyright 2026 Vsevolod Petrov (lazyhat)
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

target=${1:?target is required}
ffi_library=${2:?FFI library is required}
jni_library=${3:?JNI library is required}
RUNTIME_VERSION=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' runtime-version.toml)
RUNTIME_TAG=${4:-v$RUNTIME_VERSION}
test "$RUNTIME_TAG" = "v$RUNTIME_VERSION"
RUNTIME_ABI=${RUNTIME_VERSION#0.}
RUNTIME_ABI=${RUNTIME_ABI%%.*}
case "$target" in
    x86_64-unknown-linux-gnu) platform=linux-x86_64; extension=.tar.gz ;;
    x86_64-pc-windows-msvc) platform=windows-x86_64; extension=.zip ;;
    *) echo "Unsupported Runtime target: $target" >&2; exit 1 ;;
esac

cargo xtask check
cargo build -p compukter-ffi -p compukter-jni --release --locked --offline
cargo run -p runtime-bundler --locked --offline -- smoke --library "$ffi_library" --abi "$RUNTIME_ABI"
javac -d target/runtime-release-smoke tools/runtime-bundler/fixtures/ru/lazyhat/compukters/lang/runtime/vm/JniNative.java
java --enable-native-access=ALL-UNNAMED -cp target/runtime-release-smoke \
    ru.lazyhat.compukters.lang.runtime.vm.JniNative "$jni_library" "$RUNTIME_ABI"
cargo run -p runtime-bundler --locked --offline -- package \
    --version-file runtime-version.toml --tag "$RUNTIME_TAG" --commit "$(git rev-parse HEAD)" \
    --target "$target" --ffi-library "$ffi_library" --jni-library "$jni_library" \
    --license LICENSE --notice NOTICE --rustc "$(rustc --version)" \
    --format artifact=3 --format filesystem-generation=1 --format executable-revision=1 \
    --format compilation-request=1 --format resource-snapshot=2 --output target/runtime-release
asset="compukter-runtime-${RUNTIME_VERSION}-${platform}${extension}"
cargo run -p runtime-bundler --locked --offline -- inspect "target/runtime-release/$asset"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "asset=$asset" >> "$GITHUB_OUTPUT"
fi
