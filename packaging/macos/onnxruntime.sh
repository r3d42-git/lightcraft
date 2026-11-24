# shellcheck shell=bash
# Optional Okeinla runtime from the arm64 onnxruntime 1.23.2 wheel. The input is
# the installed wheel's `onnxruntime` directory. The dylib requires macOS 13.4.
ONNXRUNTIME_DYLIB=libonnxruntime.1.23.2.dylib

check_onnxruntime_file() {
  local path="$1" expected="$2"
  if [[ ! -f "$path" || "$(sha256 "$path")" != "$expected" ]]; then
    echo "error: missing or unexpected ONNX Runtime file: $path" >&2
    return 1
  fi
}
validate_onnxruntime() {
  local input="$LIGHTCRAFT_ONNXRUNTIME_PACKAGE"
  check_onnxruntime_file "$input/capi/$ONNXRUNTIME_DYLIB" c1440b79237783705e156956469f207d51d03d74003886197972af76b966c04c &&
    check_onnxruntime_file "$input/LICENSE" 2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c &&
    check_onnxruntime_file "$input/ThirdPartyNotices.txt" e9e90971a8e75a9a8ac0c6412e29c1202d079998389915aa485f46c816c3b4cc
}

copy_onnxruntime() {
  local library_directory="$1" license_directory="$2" executable="$3"
  local input="$LIGHTCRAFT_ONNXRUNTIME_PACKAGE"
  if [[ "$(lipo -archs "$executable")" != arm64 ]]; then
    echo "error: the pinned ONNX Runtime supports only an arm64 executable: $executable" >&2
    return 1
  fi
  mkdir -p "$library_directory" "$license_directory"
  install -m 755 "$input/capi/$ONNXRUNTIME_DYLIB" "$library_directory/$ONNXRUNTIME_DYLIB"
  install -m 644 "$input/LICENSE" "$license_directory/ONNX-Runtime-LICENSE.txt"
  install -m 644 "$input/ThirdPartyNotices.txt" "$license_directory/ONNX-Runtime-ThirdPartyNotices.txt"
}
