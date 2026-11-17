#!/usr/bin/env bash
# Download a prebuilt V8 snapshot for the given ABI from this fork's Releases.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ABI=arm64-v8a
TAG="${OBSCURA_RELEASE_TAG:-android-v0.2.7}"

while [ $# -gt 0 ]; do
  case "$1" in
    --abi) ABI="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

OUT="$ROOT/android/snapshots/obscura-snapshot-$ABI.bin"
mkdir -p "$(dirname "$OUT")"
URL="https://github.com/AcE77505/obscura/releases/download/$TAG/obscura-snapshot-$ABI.bin"
echo "Downloading $URL"
curl -fL --retry 3 -o "$OUT" "$URL"
echo "Snapshot: $OUT"
