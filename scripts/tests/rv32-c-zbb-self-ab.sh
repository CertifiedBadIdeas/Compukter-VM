#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
exec bash "$ROOT/tools/compukter-vm-benchmarks/scripts/tests/$(basename "$0")" "$@"
