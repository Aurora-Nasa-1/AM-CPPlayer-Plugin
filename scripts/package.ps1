<#
.SYNOPSIS
  Builds the provider library and packages a CPPlayer kmp-sdk-v1 module zip.

.DESCRIPTION
  The host accepts a zip with `manifest.json` plus the dynamic library at
  `lib/<abi>/<entryPoint>` (a copy at the zip root also works). This script
  loads the MSVC environment the same way `cargo` needs it on Windows, builds
  release, and writes `dist/apple-music-<platform>.zip`.

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

$abi = switch ($Platform) {
    "windows" { "x86_64" }
    "macos" { "arm64" }
    "linux" { "x86_64" }
    "android" { "arm64-v8a" }
}

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
    } else {
        cargo build --release @envArgs
    }
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

$targetDir = if ($Target) { Join-Path $root "target/$Target/release" } else { Join-Path $root "target/release" }
$built = Join-Path $targetDir $library
if (-not (Test-Path $built)) { throw "Missing build output: $built" }

$staging = Join-Path $root "dist/staging-$Platform"
if (Test-Path $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $staging "lib/$abi") -Force | Out-Null

Copy-Item -LiteralPath $built -Destination (Join-Path $staging "lib/$abi/$library")
Copy-Item -LiteralPath $built -Destination (Join-Path $staging $library)

$manifest = [ordered]@{
    id            = "apple-music-rs"
    name          = "Apple Music (Rust)"
    version       = $resolvedVersion
    type          = "jni"
    contract      = "kmp-sdk-v1"
    entryPoint    = $library
    supportedAbis = @($abi)
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
Compress-Archive -Path (Join-Path $staging "*") -DestinationPath $out -CompressionLevel Optimal

Write-Host "OK -> $out"
