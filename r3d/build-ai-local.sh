#!/usr/bin/env bash
# Optimized private arm64 bundle using the external AI store. Does not install or publish.
set -euo pipefail
source "$(dirname "$0")/dev-env.sh"
R3D_AI_INSTALLED="${R3D_AI_INSTALLED:-/Applications/LightCraft R3D.app}"
R3D_AI_ARTIFACTS="${R3D_AI_ARTIFACTS:-$R3D_PROJECT/artifacts/ai-retouch-install-20261030}"
R3D_AI_STAGING="${R3D_AI_STAGING:-$R3D_SCRATCH_ROOT/lightcraft/packaging-$R3D_INSTANCE}"
R3D_AI_APP="${R3D_AI_APP:-$R3D_SCRATCH_ROOT/lightcraft/apps-$R3D_INSTANCE/LightCraft R3D.app}"
R3D_AI_DIST="${R3D_AI_DIST:-$R3D_SCRATCH_ROOT/lightcraft/dist-$R3D_INSTANCE}"
r3d_validate_scratch_paths "$R3D_AI_STAGING" "$R3D_AI_APP" "$R3D_AI_DIST"
export LIGHTCRAFT_VERSION="${LIGHTCRAFT_VERSION:-0.15.0-r3d-ai-external}"
export LIGHTCRAFT_BUILD_SHA="$(git -C "$R3D_SOURCE" rev-parse HEAD)"
export LIGHTCRAFT_BUILD_DATE="$(date -u +%Y-%m-%d)"
export MACOSX_DEPLOYMENT_TARGET=11.0
cd "$R3D_SOURCE"
if [[ "${1:-}" != --skip-build ]]; then
  cargo build --locked --offline --release -p lightcraft -p lightcraft-cli
fi
test -x "$CARGO_TARGET_DIR/release/lightcraft"
test ! -e "$R3D_AI_STAGING"
test ! -e "$R3D_AI_APP"
mkdir -p "$R3D_AI_STAGING/debug" "$R3D_AI_ARTIFACTS"
for binary in lightcraft lightcraft-cli; do
  cp "$CARGO_TARGET_DIR/release/$binary" "$R3D_AI_ARTIFACTS/$binary"
  ln -s "$R3D_AI_ARTIFACTS/$binary" "$R3D_AI_STAGING/debug/$binary"
done
# Native packaging is reused for the optimized binaries; its dev-app switch stops
# before distribution containers/notarization. Restore the actual installed identity below.
CARGO_TARGET_DIR="$R3D_AI_STAGING" DIST="$R3D_AI_DIST" MACOS_SIGN_IDENTITY=- \
  bash packaging/macos/package.sh --arch aarch64 --skip-build --dev-app
python3 tools/ai_retouch/package_runtime.py embed \
  --worker "$R3D_SOURCE/tools/ai_retouch/worker.py" --app "$R3D_AI_STAGING/macos-package/LightCraft R3D.app"
python3 - "$R3D_AI_INSTALLED/Contents/Info.plist" "$R3D_AI_STAGING/macos-package/LightCraft R3D.app/Contents/Info.plist" <<'PY'
import os,plistlib,sys
with open(sys.argv[1],'rb') as f: info=plistlib.load(f)
assert info['CFBundleIdentifier']=='local.r3d.lightcraft'
assert info['LSEnvironment']['LIGHTCRAFT_R3D_LIBRARY']=='/Volumes/Media/LightCraft/Library'
assert 'LIGHTCRAFT_AI_RETOUCH_RUNTIME' not in info['LSEnvironment']
info.update(LightCraftVersion=os.environ['LIGHTCRAFT_VERSION'],LightCraftBuildCommit=os.environ['LIGHTCRAFT_BUILD_SHA'])
info.pop('LightCraftBuildPatchSHA256',None)
with open(sys.argv[2],'wb') as f: plistlib.dump(info,f)
PY
codesign --force --sign - --timestamp=none --options runtime \
  --entitlements packaging/macos/entitlements.plist "$R3D_AI_STAGING/macos-package/LightCraft R3D.app"
codesign --verify --strict --deep "$R3D_AI_STAGING/macos-package/LightCraft R3D.app"
python3 tools/ai_retouch/package_runtime.py copy-app \
  --source "$R3D_AI_STAGING/macos-package/LightCraft R3D.app" --destination "$R3D_AI_APP"
test -f "$R3D_AI_APP/Contents/Resources/ai-retouch-worker.py"
printf '%s\n' "$R3D_AI_APP"
