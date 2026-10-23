#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/dev-env.sh"
if [[ ! -x "$CARGO_TARGET_DIR/debug/lightcraft" ]]; then
  echo "Build first: source r3d/dev-env.sh; cargo build --locked -p lightcraft -p lightcraft-cli" >&2
  exit 1
fi
if [[ ! -f "$LIGHTCRAFT_R3D_CONFIG_DIR/optics/sources.json" ]]; then
  echo "Prepare the isolated optical data with r3d/bootstrap-optics.py first." >&2
  exit 1
fi
mkdir -p "$R3D_INPUT_DIR"
# Only the dedicated input copies are passed automatically. No recent official
# library or remembered official directory is read.
exec "$CARGO_TARGET_DIR/debug/lightcraft" --no-demo \
  --library "$LIGHTCRAFT_R3D_LIBRARY" --control "$R3D_CONTROL_PORT" "$R3D_INPUT_DIR"
