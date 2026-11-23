param(
    [string]$AndroidSdk = "$env:USERPROFILE\AppData\Local\Android\Sdk",
    [string]$Gradle = "",
    [switch]$Check,
    [switch]$Install
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$pins = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'toolchain.json') -Raw | ConvertFrom-Json
$ndk = Join-Path $AndroidSdk "ndk\$($pins.ndk)"
foreach ($required in @($ndk, (Join-Path $AndroidSdk "platforms\android-$($pins.compileSdk)"), (Join-Path $AndroidSdk "build-tools\$($pins.buildTools)"))) {
    if (-not (Test-Path -LiteralPath $required)) { throw "Missing pinned Android SDK component: $required. See mobile/README.md." }
}
$toolchain = Join-Path $ndk 'toolchains\llvm\prebuilt\windows-x86_64\bin'
if (-not $Gradle) {
    $toolsDir = Join-Path $repo 'target\android-tools'
    $Gradle = Join-Path $toolsDir "gradle-$($pins.gradle)\bin\gradle.bat"
    if (-not (Test-Path -LiteralPath $Gradle)) {
        New-Item -ItemType Directory -Path $toolsDir -Force | Out-Null
        $archive = Join-Path $toolsDir "gradle-$($pins.gradle)-bin.zip"
        if (-not (Test-Path -LiteralPath $archive)) {
            Invoke-WebRequest -Uri "https://services.gradle.org/distributions/gradle-$($pins.gradle)-bin.zip" -OutFile $archive
        }
        if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $pins.gradleSha256) {
            throw "Gradle checksum mismatch. Remove $archive and retry."
        }
        Expand-Archive -LiteralPath $archive -DestinationPath $toolsDir -Force
    }
}
$gradleVersion = & $Gradle --version
if ($LASTEXITCODE -ne 0 -or -not ($gradleVersion -match "^Gradle $([regex]::Escape($pins.gradle))$")) {
    throw "Use exactly Gradle $($pins.gradle), or omit -Gradle for a checksum-verified download."
}
$java = if ($env:JAVA_HOME) { Join-Path $env:JAVA_HOME 'bin\java.exe' } else { 'java' }
$javaVersion = & $java -version 2>&1 | Out-String
if ($LASTEXITCODE -ne 0 -or $javaVersion -notmatch ('version "' + $pins.java + '\.')) {
    throw "Use JDK $($pins.java) and set JAVA_HOME and PATH to that JDK."
}
$targetRoot = Join-Path $repo 'target\android'
$jniDir = Join-Path $PSScriptRoot 'app\src\main\jniLibs\arm64-v8a'
$variables = @('ANDROID_HOME','ANDROID_NDK_HOME','CARGO_TARGET_DIR','CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER','CC_aarch64_linux_android','CXX_aarch64_linux_android','AR_aarch64_linux_android','RUSTFLAGS')
$previous = @{}
foreach ($name in $variables) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
try {
    $env:ANDROID_HOME = $AndroidSdk
    $env:ANDROID_NDK_HOME = $ndk
    $env:CARGO_TARGET_DIR = $targetRoot
    $env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = Join-Path $toolchain 'aarch64-linux-android29-clang.cmd'
    $env:CC_aarch64_linux_android = $env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER
    $env:CXX_aarch64_linux_android = Join-Path $toolchain 'aarch64-linux-android29-clang++.cmd'
    $env:AR_aarch64_linux_android = Join-Path $toolchain 'llvm-ar.exe'
    $env:RUSTFLAGS = (($previous.RUSTFLAGS + ' -C link-arg=-Wl,-z,max-page-size=16384').Trim())
    Push-Location $repo
    try {
        cargo "+$($pins.rust)" build --locked -p veil-android --target aarch64-linux-android
        if ($LASTEXITCODE -ne 0) { throw 'Rust Android build failed.' }
        if ($Check) {
            cargo "+$($pins.rust)" clippy --locked -p veil-android --target aarch64-linux-android -- -D warnings
            if ($LASTEXITCODE -ne 0) { throw 'Android Clippy failed.' }
        }
    } finally { Pop-Location }
    New-Item -ItemType Directory -Path $jniDir -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $targetRoot 'aarch64-linux-android\debug\libveil_android.so') -Destination $jniDir
    Copy-Item -LiteralPath (Join-Path $ndk 'toolchains\llvm\prebuilt\windows-x86_64\sysroot\usr\lib\aarch64-linux-android\libc++_shared.so') -Destination $jniDir
    & (Join-Path $toolchain 'llvm-strip.exe') --strip-debug (Join-Path $jniDir 'libveil_android.so')
    if ($LASTEXITCODE -ne 0) { throw 'Cannot strip Android native library.' }
    Push-Location $PSScriptRoot
    try {
        & $Gradle --no-daemon --console=plain :app:assembleDebug
        if ($LASTEXITCODE -ne 0) { throw 'APK packaging failed.' }
    } finally { Pop-Location }
    $apk = Join-Path $PSScriptRoot 'app\build\outputs\apk\debug\app-debug.apk'
    $buildTools = Join-Path $AndroidSdk "build-tools\$($pins.buildTools)"
    & (Join-Path $buildTools 'zipalign.exe') -c -P 16 4 $apk
    if ($LASTEXITCODE -ne 0) { throw 'APK page alignment check failed.' }
    & (Join-Path $buildTools 'apksigner.bat') verify $apk
    if ($LASTEXITCODE -ne 0) { throw 'APK debug signature check failed.' }
    Push-Location $repo
    try {
        $revision = git rev-parse HEAD
        if ($LASTEXITCODE -ne 0) { throw 'Cannot record source revision.' }
        $dirty = [bool](git status --porcelain --untracked-files=normal)
    } finally { Pop-Location }
    [ordered]@{
        revision = $revision
        dirty = $dirty
        toolchain = $pins
        sha256 = (Get-FileHash -LiteralPath $apk -Algorithm SHA256).Hash.ToLowerInvariant()
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path (Split-Path $apk) 'build-info.json') -Encoding utf8
    Write-Output "Built: $apk"
    if ($Install) {
        & (Join-Path $AndroidSdk 'platform-tools\adb.exe') install -r $apk
        if ($LASTEXITCODE -ne 0) { throw 'Phone installation failed.' }
    }
} finally {
    foreach ($name in $variables) {
        if ($null -eq $previous[$name]) {
            # Passing null through the .NET string overload can create an empty variable.
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process')
        }
    }
}
