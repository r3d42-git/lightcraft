#!/usr/bin/env bash
# Isolated self-contained local test app; no production installation/release.
set -euo pipefail
source "$(dirname "$0")/dev-env.sh"
# Keep this uniquely identified test app's data in its normal internal macOS
# support folder. External-volume authorization is separate from bundle startup.
R3D_AI_TEST_DATA="${R3D_AI_TEST_DATA:-$HOME/Library/Application Support/LightCraft KI Test 20261030}"
export LIGHTCRAFT_R3D_CONFIG_DIR="$R3D_AI_TEST_DATA/config"
export LIGHTCRAFT_R3D_LIBRARY="$R3D_AI_TEST_DATA/library"
export LIGHTCRAFT_LENSFUN_DIR="$LIGHTCRAFT_R3D_CONFIG_DIR/optics/lensfun"
export LIGHTCRAFT_R3D_EXIFTOOL="$LIGHTCRAFT_R3D_CONFIG_DIR/optics/exiftool/exiftool"
R3D_AI_RUNTIME="$R3D_PROJECT/test-runtime/ai-retouch-dev"
R3D_AI_HELPER="$R3D_SOURCE/tools/ai_retouch/package_runtime.py"
R3D_AI_APP="$CARGO_TARGET_DIR/macos-package/LightCraft R3D.app"
R3D_AI_TEST_APP="$R3D_PROJECT/test-runtime/$R3D_INSTANCE/LightCraft KI Test.app"
# Native packaging recreates its output; protect both possible running copies.
python3 "$R3D_AI_HELPER" check-closed --app "$R3D_AI_APP"
python3 "$R3D_AI_HELPER" check-closed --app "$R3D_AI_TEST_APP"
test -x "$R3D_AI_RUNTIME/bin/python3"
test -f "$R3D_AI_RUNTIME/model/lightcraft-model.json"
test -f "$R3D_AI_RUNTIME/lama/manifest.json"
test -f "$R3D_AI_RUNTIME/objectclear/lightcraft-model.json"
# The engine locates Contents/Resources/ai-retouch next to its executable.
unset LIGHTCRAFT_AI_RETOUCH_RUNTIME
cd "$R3D_SOURCE"
cargo build --locked -p lightcraft -p lightcraft-cli
bash packaging/macos/package.sh --dev-app --skip-build
python3 "$R3D_AI_HELPER" embed --runtime "$R3D_AI_RUNTIME" \
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
# All embedded Mach-O leaves were individually signed by the helper.
codesign --force --sign - --timestamp=none "$R3D_AI_APP"
codesign --verify --strict --deep "$R3D_AI_APP"
python3 "$R3D_AI_HELPER" copy-app --source "$R3D_AI_APP" --destination "$R3D_AI_TEST_APP"
python3 "$R3D_AI_HELPER" validate --runtime "$R3D_AI_TEST_APP/Contents/Resources/ai-retouch"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$R3D_AI_TEST_APP"
printf '%s\n' "$R3D_AI_TEST_APP"
