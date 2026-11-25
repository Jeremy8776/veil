<#
.SYNOPSIS
  Build and sign Veil for Android phones (ARM64).

.DESCRIPTION
  Produces, in $env:DIST (default: dist):
    veil-<version>-android-arm64.apk   Veil (io.github.jeremy8776.veil), signed with Veil's
                                       Android release key

  Builds with mobile/android/build.ps1 -Release, page-aligns the APK for 16 KB pages, signs it
  with apksigner (APK signature scheme v3; no separate v4 .idsig file, so the release holds
  one file) and checks the signature.

  The release key comes from the environment, never from the repository:
    ANDROID_KEYSTORE           the keystore (.jks or .p12), base64
    ANDROID_KEYSTORE_PASSWORD  its password
    ANDROID_KEY_ALIAS          the key's alias
    ANDROID_KEY_PASSWORD       the key's password (defaults to the keystore password)
  Android installs an update only when it is signed with the same key, so this key must be
  kept for the life of the app. Without ANDROID_KEYSTORE the script stops: an unsigned APK
  cannot be installed.

  Needs what mobile/android/build.ps1 needs (pinned SDK, NDK, JDK 17, Rust).

.EXAMPLE
  pwsh packaging/android/package.ps1 -AndroidSdk $env:ANDROID_HOME
#>
param([string] $AndroidSdk = "$env:USERPROFILE\AppData\Local\Android\Sdk")
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist' }
if (-not $env:ANDROID_KEYSTORE) { throw 'ANDROID_KEYSTORE is not set: an unsigned APK cannot be installed.' }
if (-not $env:ANDROID_KEYSTORE_PASSWORD -or -not $env:ANDROID_KEY_ALIAS) {
    throw 'ANDROID_KEYSTORE_PASSWORD and ANDROID_KEY_ALIAS must be set with ANDROID_KEYSTORE.'
}
if (-not $env:ANDROID_KEY_PASSWORD) { $env:ANDROID_KEY_PASSWORD = $env:ANDROID_KEYSTORE_PASSWORD }

$cargo = Get-Content -LiteralPath (Join-Path $Root 'Cargo.toml') -Raw
if ($cargo -notmatch '(?m)^version = "(\d+\.\d+\.\d+)"') { throw 'Cannot read the workspace version.' }
$version = $Matches[1]
if ($env:VEIL_VERSION -and $env:VEIL_VERSION -ne $version) {
    throw "VEIL_VERSION $env:VEIL_VERSION does not match the workspace version $version."
}

& (Join-Path $Root 'mobile\android\build.ps1') -AndroidSdk $AndroidSdk -Release
if ($LASTEXITCODE -ne 0) { throw 'Android release build failed.' }

$pins = Get-Content -LiteralPath (Join-Path $Root 'mobile\android\toolchain.json') -Raw | ConvertFrom-Json
$buildTools = Join-Path $AndroidSdk "build-tools\$($pins.buildTools)"
$unsigned = Join-Path $Root 'mobile\android\app\build\outputs\apk\release\app-release-unsigned.apk'
New-Item -ItemType Directory -Path $Dist -Force | Out-Null
$aligned = Join-Path $env:TEMP "veil-aligned-$PID.apk"
$keystore = Join-Path $env:TEMP "veil-release-$PID.keystore"
$apk = Join-Path $Dist "veil-$version-android-arm64.apk"
try {
    & (Join-Path $buildTools 'zipalign.exe') -f -P 16 4 $unsigned $aligned
    if ($LASTEXITCODE -ne 0) { throw 'zipalign failed.' }
    [IO.File]::WriteAllBytes($keystore, [Convert]::FromBase64String($env:ANDROID_KEYSTORE))
    # Passwords go through environment variables, so they never appear on a command line.
    & (Join-Path $buildTools 'apksigner.bat') sign --ks $keystore `
        --ks-pass env:ANDROID_KEYSTORE_PASSWORD --ks-key-alias $env:ANDROID_KEY_ALIAS `
        --key-pass env:ANDROID_KEY_PASSWORD --v4-signing-enabled false --out $apk $aligned
    if ($LASTEXITCODE -ne 0) { throw 'apksigner sign failed.' }
} finally {
    Remove-Item -LiteralPath $keystore, $aligned -Force -ErrorAction SilentlyContinue
}
& (Join-Path $buildTools 'apksigner.bat') verify --verbose $apk
if ($LASTEXITCODE -ne 0) { throw 'The signed APK does not verify.' }
& (Join-Path $buildTools 'zipalign.exe') -c -P 16 4 $apk
if ($LASTEXITCODE -ne 0) { throw 'The signed APK is not page-aligned.' }
Write-Output "Built: $apk"
