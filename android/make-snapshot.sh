#!/usr/bin/env bash
# Generate an architecture-correct V8 snapshot on an arm64/x64 Linux or macOS host.
#
# Uses the clean upstream source because the android branch removes the
# build-dependency that obscura-js/build.rs needs to generate a snapshot.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UPSTREAM_TAG="${UPSTREAM_TAG:-v0.2.7}"
MACHINE="$(uname -m)"
ABI=arm64-v8a
if [ "$MACHINE" = "x86_64" ]; then ABI=x86_64; fi

while [ $# -gt 0 ]; do
  case "$1" in
    --abi) ABI="$2"; shift 2 ;;
    --tag) UPSTREAM_TAG="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) echo "Windows cannot generate an arm64 snapshot; run this on arm64 Linux/macOS or use Releases." >&2; exit 1 ;;
esac

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
git -C "$ROOT" archive "$UPSTREAM_TAG" | tar -x -C "$WORK"
echo "Building obscura-js natively for $ABI..."
(cd "$WORK" && cargo build --release -p obscura-js)

SNAP=$(find "$WORK/target/release/build" -path '*obscura-js*/out/OBSCURA_SNAPSHOT.bin' | head -1)
if [ -z "$SNAP" ]; then
  echo "Snapshot was not produced" >&2
  exit 1
fi
OUT="$ROOT/android/snapshots/obscura-snapshot-$ABI.bin"
mkdir -p "$(dirname "$OUT")"
cp "$SNAP" "$OUT"
echo "Snapshot: $OUT"
