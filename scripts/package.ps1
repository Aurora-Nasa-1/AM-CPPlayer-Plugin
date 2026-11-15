<#
.SYNOPSIS
  Builds the provider library and packages a CPPlayer kmp-sdk-v1 module zip.

.DESCRIPTION
  The host accepts a zip with `manifest.json` plus the dynamic library at
  `lib/<abi>/<entryPoint>` (a copy at the zip root also works). This script
  loads the MSVC environment the same way `cargo` needs it on Windows, builds
  release, and writes `dist/apple-music-<platform>.zip`.

  Packages are published as `native`: the library exports both the JNI
  `NativeSession` symbols and the C ABI v1 (`cpp_*`) symbols. CPPlayer >= 1.9
  prefers the C ABI and rejects `type: "jni"` catalog entries.

.EXAMPLE
  pwsh -File scripts/package.ps1
  pwsh -File scripts/package.ps1 -Platform linux -Target x86_64-unknown-linux-gnu
#>
[CmdletBinding()]
param(
    [ValidateSet("windows", "linux", "macos", "android")]
    [string]$Platform = "windows",
    [string]$Target = "",
    [string]$Version = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$library = @{
    windows = "am_sdk_provider.dll"
    linux   = "libam_sdk_provider.so"
    macos   = "libam_sdk_provider.dylib"
    android = "libam_sdk_provider.so"
}[$Platform]

$runtime = "native"

# Host-side directory name (not the catalog arch token).
$libAbi = @{
    windows = "x86_64"
    macos   = "arm64"
    linux   = "x86_64"
    android = "arm64-v8a"
}[$Platform]

function Get-CrateVersion {
    $content = Get-Content (Join-Path $root "Cargo.toml") -Raw
    if ($content -match '(?m)^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    throw "Cargo.toml has no version"
}

$resolvedVersion = if ($Version) { $Version } else { Get-CrateVersion }

if (-not $SkipBuild) {
    $envArgs = if ($Target) { @("--target", $Target) } else { @() }
    if ($Platform -eq "windows") {
        $vcvars = "E:\project\build\VC\Auxiliary\Build\vcvars64.bat"
        if (Test-Path $vcvars) {
            # cmd is required: vcvars64.bat is a batch script that only exports
            # environment variables into its own process.
            cmd /c "`"$vcvars`" >nul 2>&1 && cargo build --release $($envArgs -join ' ')"
        } else {
            cargo build --release @envArgs
        }
    } elseif ($Platform -eq "android") {
        $sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } elseif ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT } else { "" }
        $ndkRoot = if ($env:ANDROID_NDK_HOME) { $env:ANDROID_NDK_HOME } else { "" }
        if (-not $ndkRoot -and $sdk) {
            $ndkRoot = Get-ChildItem (Join-Path $sdk "ndk") -Directory -ErrorAction SilentlyContinue |
                Where-Object { Test-Path (Join-Path $_.FullName "toolchains/llvm") } |
                Sort-Object Name -Descending | Select-Object -First 1 -ExpandProperty FullName
        }
        if ($ndkRoot -and -not (Test-Path (Join-Path $ndkRoot "toolchains/llvm"))) {
            $ndkRoot = $null
        }
        if (-not $ndkRoot) { throw "Android NDK not found; set ANDROID_NDK_HOME or install one under ANDROID_HOME/ndk" }
        $hostTag = if ($PSVersionTable.PSEdition -eq "Core") {
            if ($IsWindows) { "windows-x86_64" } elseif ($IsMacOS) { "darwin-x86_64" } else { "linux-x86_64" }
        } else {
            "windows-x86_64"
        }
        $ndkBin = Join-Path $ndkRoot "toolchains/llvm/prebuilt/$hostTag/bin"
        $clang = Join-Path $ndkBin "aarch64-linux-android21-clang"
        if (Test-Path "$clang.cmd") { $clang = "$clang.cmd" }
        if (-not (Test-Path $clang)) { throw "Android clang not found: $clang" }
        $env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $clang
        $env:CC_aarch64_linux_android = $clang
        $env:AR_aarch64_linux_android = Join-Path $ndkBin "llvm-ar"
        cargo build --release @envArgs
    } else {
        cargo build --release @envArgs
    }
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

$targetDir = if ($Target) { Join-Path $root "target/$Target/release" } else { Join-Path $root "target/release" }
$built = Join-Path $targetDir $library
if (-not (Test-Path $built)) { throw "Missing build output: $built" }

if ($Platform -eq "windows") {
    # A build that only exports JNI would install but fail at call time on the
    # GraalVM native host. Fail the package instead of shipping it.
    $exports = cmd /c "`"E:\project\build\VC\Auxiliary\Build\vcvars64.bat`" >nul 2>&1 && dumpbin /EXPORTS `"$built`""
    foreach ($symbol in @("cpp_abi_version", "cpp_create", "cpp_invoke", "cpp_cancel", "cpp_close")) {
        if (-not ($exports -match "\b$symbol\b")) {
            throw "Missing C ABI symbol $symbol in $built"
        }
    }
}

$staging = Join-Path $root "dist/staging-$Platform"
if (Test-Path $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $staging "lib/$libAbi") -Force | Out-Null

Copy-Item -LiteralPath $built -Destination (Join-Path $staging "lib/$libAbi/$library")
Copy-Item -LiteralPath $built -Destination (Join-Path $staging $library)

$entryPoint = $library
$supportedAbis = @($libAbi)
if ($Platform -eq "android") { $entryPoint = "lib/$libAbi/$library" }

$manifest = [ordered]@{
    id            = "apple-music-rs"
    name          = "Apple Music (Rust)"
    version       = $resolvedVersion
    type          = $runtime
    contract      = "kmp-sdk-v1"
    entryPoint    = $entryPoint
    supportedAbis = $supportedAbis
    sdkOrigins    = @("https://amp-api.music.apple.com", "https://music.apple.com")
    capabilities  = @("search", "playback", "drmPlayback", "lyricsSearch", "userPlaylists", "artistExplorer", "albumExplorer")
    qualityOptions = @(
        [ordered]@{ key = "drm-standard"; label = "AAC (DRM)"; targetQuality = "standard" },
        [ordered]@{ key = "drm-lossless"; label = "ALAC Lossless (DRM)"; targetQuality = "lossless" },
        [ordered]@{ key = "preview"; label = "Preview (clear AAC)"; targetQuality = "standard" }
    )
    settingsSchema = @(
        [ordered]@{ key = "storefront"; label = "Storefront"; type = "string"; default = "us" },
        [ordered]@{ key = "language"; label = "Language"; type = "string"; default = "en-US" },
        [ordered]@{ key = "allowPreview"; label = "Allow preview playback"; type = "switch"; default = "true" }
    )
}

$manifestPath = Join-Path $staging "manifest.json"
$json = $manifest | ConvertTo-Json -Depth 8
[System.IO.File]::WriteAllText($manifestPath, $json + "`n", [System.Text.UTF8Encoding]::new($false))

$out = Join-Path $root "dist/apple-music-$Platform.zip"
if (Test-Path $out) { Remove-Item -LiteralPath $out -Force }
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $staging,
    $out,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $false
)

Write-Host "OK -> $out"
