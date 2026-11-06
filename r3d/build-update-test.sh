#!/usr/bin/env bash
# Optimized isolated update test; native packaging, no install or publication.
set -euo pipefail
source "$(dirname "$0")/dev-env.sh"
R3D_TEST_ROOT="${R3D_TEST_ROOT:-$R3D_SCRATCH_ROOT/lightcraft/update-0.18.1-test-20261106}"
R3D_TEST_NAME='LightCraft R3D 0.18.1 Test'
export LIGHTCRAFT_R3D_APP_NAME="$R3D_TEST_NAME"
export LIGHTCRAFT_VERSION='0.18.1-r3d-test'
export LIGHTCRAFT_BUILD_SHA="$(git -C "$R3D_SOURCE" rev-parse HEAD)"
export LIGHTCRAFT_BUILD_DATE="$(date -u +%Y-%m-%d)"
export LIGHTCRAFT_R3D_LIBRARY="$R3D_TEST_ROOT/fixture/library"
export LIGHTCRAFT_R3D_CONFIG_DIR="$R3D_TEST_ROOT/fixture/config"
export LIGHTCRAFT_AI_HOME="$R3D_TEST_ROOT/fixture/ai"
export LIGHTCRAFT_SAM3_DIR="$LIGHTCRAFT_AI_HOME/models/sam3/3c879f39826c281e95690f02c7821c4de09afae7"
export LIGHTCRAFT_LENSFUN_DIR="$LIGHTCRAFT_R3D_CONFIG_DIR/optics/lensfun"
export LIGHTCRAFT_R3D_EXIFTOOL="$LIGHTCRAFT_R3D_CONFIG_DIR/optics/exiftool/exiftool"
export PYTHONDONTWRITEBYTECODE=1
export CRAFT_FONTS_DIR="${CRAFT_FONTS_DIR:-$R3D_SCRATCH_ROOT/lightcraft/dependencies/craft-fonts}"
export CRAFT_FONTS_REQUIRED=1
test -f "$CRAFT_FONTS_DIR/fonts/manifest.txt"
unset LIGHTCRAFT_AI_RETOUCH_RUNTIME
R3D_TEST_STAGE="$R3D_TEST_ROOT/package-stage"
R3D_TEST_APP="$R3D_TEST_ROOT/$R3D_TEST_NAME.app"
r3d_validate_scratch_paths "$R3D_TEST_ROOT" "$R3D_TEST_STAGE" "$R3D_TEST_APP"
python3 "$R3D_SOURCE/tools/ai_retouch/package_runtime.py" check-closed --app "$R3D_TEST_APP"
cd "$R3D_SOURCE"
if [[ "${1:-}" != --skip-build ]]; then
  cargo build --locked --offline --release -p lightcraft -p lightcraft-cli
fi
test ! -e "$R3D_TEST_STAGE"
mkdir -p "$R3D_TEST_STAGE/debug" "$R3D_TEST_ROOT/bin"
for binary in lightcraft lightcraft-cli; do
  /bin/cp "$CARGO_TARGET_DIR/release/$binary" "$R3D_TEST_ROOT/bin/$binary"
  ln -s "$R3D_TEST_ROOT/bin/$binary" "$R3D_TEST_STAGE/debug/$binary"
done
CARGO_TARGET_DIR="$R3D_TEST_STAGE" DIST="$R3D_TEST_ROOT/package-dist" MACOS_SIGN_IDENTITY=- \
  bash packaging/macos/package.sh --arch aarch64 --skip-build --dev-app
R3D_PACKAGED_APP="$R3D_TEST_STAGE/macos-package/LightCraft R3D.app"
python3 tools/ai_retouch/package_runtime.py embed --worker "$R3D_SOURCE/tools/ai_retouch/worker.py" --app "$R3D_PACKAGED_APP"
python3 - "$R3D_PACKAGED_APP/Contents/Info.plist" "$R3D_TEST_NAME" <<'PY'
import os,plistlib,sys
p=sys.argv[1]
with open(p,'rb') as f: info=plistlib.load(f)
info.update(CFBundleName=sys.argv[2],CFBundleDisplayName=sys.argv[2],CFBundleIdentifier='local.r3d.lightcraft.update-0181-test')
for k in ('CFBundleDocumentTypes','UTExportedTypeDeclarations','UTImportedTypeDeclarations','CFBundleURLTypes'):
    info.pop(k,None)
info['LSEnvironment']={k:os.environ[k] for k in ('LIGHTCRAFT_R3D_LIBRARY','LIGHTCRAFT_R3D_CONFIG_DIR','LIGHTCRAFT_AI_HOME','LIGHTCRAFT_SAM3_DIR','LIGHTCRAFT_LENSFUN_DIR','LIGHTCRAFT_R3D_EXIFTOOL','PYTHONDONTWRITEBYTECODE')}
with open(p,'wb') as f: plistlib.dump(info,f)
PY
codesign --force --sign - --timestamp=none --options runtime --entitlements packaging/macos/entitlements.plist "$R3D_PACKAGED_APP"
codesign --verify --strict --deep "$R3D_PACKAGED_APP"
python3 tools/ai_retouch/package_runtime.py copy-app --source "$R3D_PACKAGED_APP" --destination "$R3D_TEST_APP"
printf '%s\n' "$R3D_TEST_APP"
