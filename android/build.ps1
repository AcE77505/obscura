param(
    [string]$Abi = "arm64-v8a",
    [string]$Api = "29",
    [string]$Snapshot = "",
    [string]$Features = "render,stealth"
)

# Windows build wrapper. Requires Git Bash (bash.exe in PATH), Rust, Android NDK,
# Go, LLVM (libclang), CMake, Python and curl.
# Example:
#   .\android\build.ps1 -Abi arm64-v8a
#   $env:ANDROID_NDK_HOME = "D:/Android/Sdk/ndk/28.2.13676358"
#   $env:LIBCLANG_PATH = "C:/Program Files/LLVM/bin"

$bash = Get-Command bash.exe -ErrorAction SilentlyContinue
if (-not $bash) {
    throw "Git Bash (bash.exe) is required. Install Git for Windows and run this from PowerShell."
}

$argsList = @("android/build.sh", "--abi", $Abi, "--api", $Api)
if ($Snapshot) { $argsList += @("--snapshot", $Snapshot) }
if ($Features) { $argsList += @("--features", $Features) }

& $bash.Source @argsList
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
