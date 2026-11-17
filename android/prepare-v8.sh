#!/usr/bin/env bash
# Download the Android V8 prebuilt archive and merge the three shim objects
# (simdutf__* forwarders, __libcpp_verbose_abort, compiler-rt clear_cache).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ABI=arm64-v8a
API=29
NDK="${ANDROID_NDK_HOME:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --abi) ABI="$2"; shift 2 ;;
    --api) API="$2"; shift 2 ;;
    --ndk) NDK="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

case "$ABI" in
  arm64-v8a) TARGET=aarch64-linux-android; ARCH=aarch64 ;;
  x86_64) TARGET=x86_64-linux-android; ARCH=x86_64 ;;
  *) echo "unsupported --abi: $ABI" >&2; exit 1 ;;
esac

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) HOST=windows; EXE=.exe ;;
  Darwin*) HOST=macos; EXE= ;;
  *) HOST=linux; EXE= ;;
esac

case "$HOST" in
  windows) NDK_HOST=windows-x86_64 ;;
  macos) NDK_HOST=darwin-x86_64 ;;
  *) NDK_HOST=linux-x86_64 ;;
esac
if [ -z "$NDK" ] || [ ! -d "$NDK" ]; then
  echo "Android NDK not found. Set ANDROID_NDK_HOME or pass --ndk." >&2
  exit 1
fi
NDK_BIN="$NDK/toolchains/llvm/prebuilt/$NDK_HOST/bin"
AR="$NDK_BIN/llvm-ar$EXE"
NM="$NDK_BIN/llvm-nm$EXE"
if [ "$HOST" = windows ]; then
  CXX="$NDK_BIN/clang++.exe --target=${TARGET}${API}"
else
  CXX="$NDK_BIN/${TARGET}${API}-clang++"
fi

V8_VERSION=$(awk -F'"' '/^name = "v8"$/{f=1} f&&/^version = /{print $2; exit}' "$ROOT/Cargo.lock")
if [ -z "$V8_VERSION" ]; then
  echo "Cannot read the v8 version from Cargo.lock" >&2
  exit 1
fi

OUT="$ROOT/android/build/v8/$ABI"
mkdir -p "$OUT"
BASE="https://github.com/satouriko/rusty-v8-mobile/releases/download/v$V8_VERSION"

if [ ! -f "$OUT/librusty_v8.a" ]; then
  echo "Downloading V8 $V8_VERSION for $TARGET..."
  curl -fL --retry 3 -o "$OUT/librusty_v8.a.gz" "$BASE/librusty_v8_release_${TARGET}.a.gz"
  gzip -dc "$OUT/librusty_v8.a.gz" > "$OUT/librusty_v8.a"
fi
if [ ! -f "$OUT/src_binding.rs" ]; then
  curl -fL --retry 3 -o "$OUT/src_binding.rs" "$BASE/src_binding_release_${TARGET}.rs"
fi

SHIM="$OUT/shim"
mkdir -p "$SHIM"
cd "$SHIM"
"$AR" x "$OUT/librusty_v8.a" simdutf.o
"$NM" simdutf.o > simdutf.syms
python3 "$ROOT/android/shims/gen_simdutf_shim.py" simdutf.syms simdutf_shim.cc
$CXX -c simdutf_shim.cc -o simdutf_shim.o -O2 -fPIC -std=c++17
$CXX -c "$ROOT/android/shims/abort_shim.cc" -o abort_shim.o -O2 -fPIC -std=c++17

CRT=$(ls "$NDK_BIN/../lib/clang"/*/lib/linux/libclang_rt.builtins-${ARCH}-android.a 2>/dev/null | head -1)
if [ -z "$CRT" ]; then
  echo "compiler-rt builtins archive not found for $ARCH" >&2
  exit 1
fi
"$AR" x "$CRT" clear_cache.c.o

cp "$OUT/librusty_v8.a" "$OUT/librusty_v8_android.a"
"$AR" q "$OUT/librusty_v8_android.a" simdutf_shim.o abort_shim.o clear_cache.c.o
"$AR" s "$OUT/librusty_v8_android.a"
echo "Prepared: $OUT/librusty_v8_android.a"
