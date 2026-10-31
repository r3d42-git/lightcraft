#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
source "$ROOT/r3d/dev-env.sh"
OUT="$R3D_SCRATCH_ROOT/lightcraft/native-model-setup"
r3d_validate_scratch_paths "$OUT"
mkdir -p "$OUT"
xcrun swiftc -O -target arm64-apple-macos13.0 -framework AppKit -framework CryptoKit "$ROOT/tools/ai_retouch/native-setup/main.swift" -o "$OUT/lightcraft-model-setup"
# The portable executable and reviewed recipes travel together; no weights/runtime.
rm -rf "$OUT/model-recipes"
cp -R "$ROOT/tools/ai_retouch/model-recipes" "$OUT/model-recipes"
printf '%s\n' "$OUT/lightcraft-model-setup"
