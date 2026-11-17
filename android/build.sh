#!/usr/bin/env bash
# Build obscura for Android (arm64-v8a or x86_64).
#
#   ./android/build.sh --abi arm64-v8a
#   ./android/build.sh --abi x86_64 --snapshot /path/to/obscura-snapshot-x64.bin
#
# The script downloads/prepares the V8 archive, so you do not need to build V8.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ABI=arm64-v8a
API=29
FEATURES=render,stealth
SNAPSHOT=""
NDK="${ANDROID_NDK_HOME:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --abi) ABI="$2"; shift 2 ;;
    --api) API="$2"; shift 2 ;;
    --features) FEATURES="$2"; shift 2 ;;
    --snapshot) SNAPSHOT="$2"; shift 2 ;;
    --ndk) NDK="$2"; shift 2 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

case "$ABI" in
  arm64-v8a) TARGET=aarch64-linux-android; ARCH=aarch64 ;;
  x86_64) TARGET=x86_64-linux-android; ARCH=x86_64 ;;
  *) echo "unsupported --abi: $ABI (arm64-v8a|x86_64)" >&2; exit 1 ;;
esac

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) HOST=windows; EXE=.exe ;;
  Darwin*) HOST=macos; EXE= ;;
  *) HOST=linux; EXE= ;;
esac

if [ -z "$NDK" ]; then
  for base in "${ANDROID_SDK_ROOT:-}" "${ANDROID_HOME:-}" "$HOME/Android/Sdk" "$HOME/Library/Android/sdk"; do
    if [ -n "$base" ] && [ -d "$base/ndk" ]; then
      NDK="$(ls -d "$base"/ndk/* 2>/dev/null | sort -V | tail -1)"
    fi
  done
fi
if [ -z "$NDK" ] || [ ! -d "$NDK" ]; then
  echo "Android NDK not found. Set ANDROID_NDK_HOME or pass --ndk." >&2
  exit 1
fi
export ANDROID_NDK_HOME="$NDK"

case "$HOST" in
  windows) NDK_HOST=windows-x86_64 ;;
  macos) NDK_HOST=darwin-x86_64 ;;
  *) NDK_HOST=linux-x86_64 ;;
esac
NDK_BIN="$NDK/toolchains/llvm/prebuilt/$NDK_HOST/bin"
NDK_SYSROOT="$NDK/toolchains/llvm/prebuilt/$NDK_HOST/sysroot"
if [ ! -d "$NDK_BIN" ]; then
  echo "NDK toolchain not found: $NDK_BIN" >&2
  exit 1
fi

V8_DIR="$ROOT/android/build/v8/$ABI"
if [ ! -f "$V8_DIR/librusty_v8_android.a" ]; then
  "$ROOT/android/prepare-v8.sh" --abi "$ABI" --api "$API" --ndk "$NDK"
fi
export RUSTY_V8_ARCHIVE="$V8_DIR/librusty_v8_android.a"
export RUSTY_V8_SRC_BINDING_PATH="$V8_DIR/src_binding.rs"

if [ -z "$SNAPSHOT" ]; then
  SNAPSHOT="$ROOT/android/snapshots/obscura-snapshot-$ABI.bin"
fi
if [ ! -s "$SNAPSHOT" ]; then
  "$ROOT/android/download-snapshot.sh" --abi "$ABI" || {
    echo "No snapshot at $SNAPSHOT." >&2
    echo "Run android/make-snapshot.sh on an arm64/x64 Linux or macOS host, or download it from Releases." >&2
    exit 1
  }
fi
export OBSCURA_SNAPSHOT_FILE="$SNAPSHOT"

TOOLCHAIN="$ROOT/android/build/toolchains/android-$ABI.cmake"
mkdir -p "$(dirname "$TOOLCHAIN")"
cat > "$TOOLCHAIN" <<CMAKE
set(ANDROID_ABI "$ABI" CACHE STRING "")
set(ANDROID_PLATFORM "android-$API" CACHE STRING "")
set(ANDROID_NDK "$NDK" CACHE PATH "")
include("$NDK/build/cmake/android.toolchain.cmake")
CMAKE
export CMAKE_TOOLCHAIN_FILE="$TOOLCHAIN"

export CXXSTDLIB=c++_static
export BORING_BSSL_RUST_CPPLIB=c++_static
GO_EXECUTABLE="${GO_EXECUTABLE:-$(command -v go || true)}"
if [ -z "$GO_EXECUTABLE" ]; then
  echo "Go is required (BoringSSL). Install Go and put it in PATH." >&2
  exit 1
fi
export GO_EXECUTABLE

if [ -z "${LIBCLANG_PATH:-}" ]; then
  if command -v llvm-config >/dev/null 2>&1; then
    LIBCLANG_PATH="$(llvm-config --libdir)"
  elif [ "$HOST" = windows ]; then
    for d in "/c/Program Files/LLVM/bin" "/d/Program Files/LLVM/bin" "$HOME/Downloads/LLVM/bin"; do
      if [ -f "$d/libclang.dll" ]; then LIBCLANG_PATH="$d"; fi
    done
  fi
fi
if [ -z "${LIBCLANG_PATH:-}" ]; then
  echo "libclang not found. Set LIBCLANG_PATH to the directory containing libclang.so/.dylib/libclang.dll." >&2
  exit 1
fi
export LIBCLANG_PATH
export BINDGEN_EXTRA_CLANG_ARGS="--target=${TARGET}${API} --sysroot=$NDK_SYSROOT -isystem $NDK_SYSROOT/usr/include -isystem $NDK_SYSROOT/usr/include/$TARGET"

TARGET_UPPER=$(echo "$TARGET" | tr 'a-z-' 'A-Z_')

if [ "$HOST" = windows ]; then
  WRAP="$ROOT/android/build/ndkwrap"
  mkdir -p "$WRAP"
  [ -f "$WRAP/${TARGET}${API}-clang.exe" ] || cp "$NDK_BIN/clang.exe" "$WRAP/${TARGET}${API}-clang.exe"
  [ -f "$WRAP/${TARGET}${API}-clang++.exe" ] || cp "$NDK_BIN/clang++.exe" "$WRAP/${TARGET}${API}-clang++.exe"
  export "CC_${TARGET//-/_}=$NDK_BIN/clang.exe"
  export "CXX_${TARGET//-/_}=$NDK_BIN/clang++.exe"
  export "CFLAGS_${TARGET//-/_}=--target=${TARGET}${API}"
  export "CXXFLAGS_${TARGET//-/_}=--target=${TARGET}${API}"
  export "CARGO_TARGET_${TARGET_UPPER}_LINKER=$WRAP/${TARGET}${API}-clang.exe"
  CLANG_LIB_DIR=$(ls -d "$NDK_BIN/../lib/clang"/*/lib/linux/$ARCH 2>/dev/null | head -1)
  export "CARGO_TARGET_${TARGET_UPPER}_RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=--sysroot=$NDK_SYSROOT -Clink-arg=-L$CLANG_LIB_DIR"
else
  export "CC_${TARGET//-/_}=$NDK_BIN/${TARGET}${API}-clang"
  export "CXX_${TARGET//-/_}=$NDK_BIN/${TARGET}${API}-clang++"
  export "CARGO_TARGET_${TARGET_UPPER}_LINKER=$NDK_BIN/${TARGET}${API}-clang"
  export "CARGO_TARGET_${TARGET_UPPER}_RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=16384"
fi
export "AR_${TARGET//-/_}=$NDK_BIN/llvm-ar$EXE"

cd "$ROOT"
CARGO_ARGS=(--profile release-dist --target "$TARGET" -p obscura-cli --bin obscura)
if [ -n "$FEATURES" ]; then CARGO_ARGS+=(--features "$FEATURES"); fi
cargo build "${CARGO_ARGS[@]}"

OUT="$ROOT/android/dist/$ABI"
mkdir -p "$OUT"
cp "target/$TARGET/release-dist/obscura" "$OUT/libobscura.so"
chmod 755 "$OUT/libobscura.so" 2>/dev/null || true
echo "Built: $OUT/libobscura.so"
