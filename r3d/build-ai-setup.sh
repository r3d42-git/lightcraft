#!/usr/bin/env bash
# Developer entry point; no app/store changes or publication.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "$ROOT/r3d/dev-env.sh"
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "Usage: $0 REVIEWED_RUNTIME_ROOT [NEW_SCRATCH_OUTPUT]" >&2
  exit 2
fi
OUT="${2:-$R3D_SCRATCH_ROOT/lightcraft/optional-ai-setup-candidate}"
r3d_validate_scratch_paths "$OUT"
"$ROOT/tools/ai_retouch/native-setup/build.sh"
python3 "$ROOT/tools/ai_retouch/stage_setup_kit.py" \
  --runtime-root "$1" \
  --helper "$R3D_SCRATCH_ROOT/lightcraft/native-model-setup/lightcraft-model-setup" \
  --output "$OUT"
