#!/usr/bin/env bash
# Isolated slim local test app; no production installation/release.
set -euo pipefail
source "$(dirname "$0")/dev-env.sh"
# Keep this uniquely identified test app and its temporary data on scratch.
R3D_AI_TEST_DATA="${R3D_AI_TEST_DATA:-$R3D_SCRATCH_ROOT/lightcraft/test-data-$R3D_INSTANCE}"
export LIGHTCRAFT_R3D_CONFIG_DIR="$R3D_AI_TEST_DATA/config"
export LIGHTCRAFT_R3D_LIBRARY="$R3D_AI_TEST_DATA/library"
export LIGHTCRAFT_LENSFUN_DIR="$LIGHTCRAFT_R3D_CONFIG_DIR/optics/lensfun"
export LIGHTCRAFT_R3D_EXIFTOOL="$LIGHTCRAFT_R3D_CONFIG_DIR/optics/exiftool/exiftool"
R3D_AI_HELPER="$R3D_SOURCE/tools/ai_retouch/package_runtime.py"
R3D_AI_APP="$CARGO_TARGET_DIR/macos-package/LightCraft R3D.app"
R3D_AI_TEST_APP="${R3D_AI_TEST_APP:-$R3D_SCRATCH_ROOT/lightcraft/test-apps/$R3D_INSTANCE/LightCraft KI Test.app}"
r3d_validate_scratch_paths "$R3D_AI_APP" "$R3D_AI_TEST_APP" "$R3D_AI_TEST_DATA"
# Native packaging recreates its output; protect both possible running copies.
python3 "$R3D_AI_HELPER" check-closed --app "$R3D_AI_APP"
python3 "$R3D_AI_HELPER" check-closed --app "$R3D_AI_TEST_APP"
# The runtime and models are resolved from the external package store.
unset LIGHTCRAFT_AI_RETOUCH_RUNTIME
cd "$R3D_SOURCE"
cargo build --locked -p lightcraft -p lightcraft-cli
bash packaging/macos/package.sh --dev-app --skip-build
python3 "$R3D_AI_HELPER" embed \
  --worker "$R3D_SOURCE/tools/ai_retouch/worker.py" --app "$R3D_AI_APP"
python3 - "$R3D_AI_APP/Contents/Info.plist" <<'PY'
import plistlib, sys
path = sys.argv[1]
with open(path, 'rb') as f:
    data = plistlib.load(f)
data['CFBundleIdentifier'] = 'local.r3d.lightcraft.ai-retouch-dev'
data['CFBundleDisplayName'] = 'LightCraft KI Test'
data.setdefault('LSEnvironment', {}).pop('LIGHTCRAFT_AI_RETOUCH_RUNTIME', None)
with open(path, 'wb') as f:
    plistlib.dump(data, f)
PY
# The app contains the worker only; runtime signatures remain untouched.
codesign --force --sign - --timestamp=none "$R3D_AI_APP"
codesign --verify --strict --deep "$R3D_AI_APP"
python3 "$R3D_AI_HELPER" copy-app --source "$R3D_AI_APP" --destination "$R3D_AI_TEST_APP"
test -f "$R3D_AI_TEST_APP/Contents/Resources/ai-retouch-worker.py"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$R3D_AI_TEST_APP"
printf '%s\n' "$R3D_AI_TEST_APP"
