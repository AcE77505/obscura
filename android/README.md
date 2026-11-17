# obscura for Android

This branch builds [obscura](https://github.com/h4ckf0r0day/obscura) for Android
(`arm64-v8a` and `x86_64`). Upstream does not build for Android as-is; the
Android-specific changes are listed at the bottom.

## 1. Use a prebuilt binary

Download from [Releases](https://github.com/AcE77505/obscura/releases):

```text
libobscura-arm64-v8a.so
libobscura-x86_64.so
obscura-snapshot-arm64-v8a.bin
obscura-snapshot-x86_64.bin
```

Copy `libobscura-<abi>.so` to `app/src/main/jniLibs/<abi>/libobscura.so`.
No build required.

## 2. Build with GitHub Actions

Actions → **Android build** → **Run workflow** → download the
`obscura-android-<abi>` artifact. The workflow also produces the snapshots.

## 3. Build locally

### Requirements

- Rust with the `aarch64-linux-android` / `x86_64-linux-android` targets
- Android NDK (`ANDROID_NDK_HOME`)
- Go (BoringSSL build)
- CMake and Ninja
- LLVM with `libclang` (`LIBCLANG_PATH` on Windows; `llvm-config` on Linux/macOS)
- Python 3, `curl`, `gzip`
- Linux/macOS: a C++ toolchain for `btls-sys`
- Windows: Git Bash (the `build.ps1` wrapper calls `build.sh`)

### Linux / macOS / WSL

```bash
./android/build.sh --abi arm64-v8a
# or
./android/build.sh --abi x86_64
```

### Windows

```powershell
$env:ANDROID_NDK_HOME = "D:/Android/Sdk/ndk/28.2.13676358"
$env:LIBCLANG_PATH = "C:/Program Files/LLVM/bin"
.\android\build.ps1 -Abi arm64-v8a
```

The script downloads the Android V8 archive, merges the `simdutf`/`abort`/
`clear_cache` shims, and builds the binary. You do **not** need to build V8.

### Snapshots

V8 snapshots are architecture-specific. The scripts look for
`android/snapshots/obscura-snapshot-<abi>.bin` and otherwise try to download it
from Releases. If it is unavailable, generate it on an arm64/x64 Linux or macOS
host:

```bash
./android/make-snapshot.sh --abi arm64-v8a
```

Windows cannot generate an arm64 snapshot; use a Release snapshot or CI.

### Output

```text
android/dist/<abi>/libobscura.so
```

## What changed from upstream

1. `crates/obscura-js/Cargo.toml`: removed `[build-dependencies] deno_core`.
2. `crates/obscura-js/build.rs`: reads `OBSCURA_SNAPSHOT_FILE` instead of
   generating a host snapshot during cross-compilation.
3. `android/patched/v8`: `RUSTY_V8_ARCHIVE` / `RUSTY_V8_SRC_BINDING_PATH` only
   apply to Android targets, so the host build script does not try to download
   a Windows/Linux V8 archive.
4. `android/patched/deno_core`: Android `errno_location()` uses bionic's
   `__errno()`.
5. Android V8 archive: `simdutf__*` C shims (43 functions),
   `__libcpp_verbose_abort`, and compiler-rt's `clear_cache.o` are merged by
   `android/prepare-v8.sh`.
6. Build flags: NDK API 29, `c++_static`, BoringSSL CMake toolchain, 16 KB page
   alignment, and the NDK sysroot/libunwind paths.

The `android/patched` crates are pinned to the versions in `Cargo.lock`
(v8 150.4.0, deno_core 0.412.0). Update them together when upgrading obscura.
