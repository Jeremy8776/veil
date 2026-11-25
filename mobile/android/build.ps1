param(
    [string]$AndroidSdk = "$env:USERPROFILE\AppData\Local\Android\Sdk",
    [string]$Gradle = "",
    [switch]$Check,
    [switch]$Install,
    # Builds for the x86_64 Android emulator instead of ARM64 phones, and installs to it.
    [switch]$Emulator,
    # An optimised, unsigned release APK (io.github.jeremy8776.veil) for
    # packaging/android/package.ps1 to sign. Cannot be combined with -Install.
    [switch]$Release
)
$ErrorActionPreference = 'Stop'
if ($Release -and $Install) { throw 'A release APK is unsigned; sign it with packaging/android/package.ps1.' }
$profileDir = if ($Release) { 'release' } else { 'debug' }
$variant = if ($Release) { 'Release' } else { 'Debug' }
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
# java prints its version on stderr. Windows PowerShell 5.1 turns redirected native stderr into
# error records, which 'Stop' would make fatal, so this one call continues past them.
$javaVersion = & { $ErrorActionPreference = 'Continue'; & $java -version 2>&1 | Out-String }
if ($LASTEXITCODE -ne 0 -or $javaVersion -notmatch ('version "' + $pins.java + '\.')) {
    throw "Use JDK $($pins.java) and set JAVA_HOME and PATH to that JDK."
}
$targetRoot = Join-Path $repo 'target\android'
# Phones are ARM64. The emulator build is x86_64, so the emulator runs it without ARM translation.
$rustTarget = if ($Emulator) { $pins.emulatorTarget } else { 'aarch64-linux-android' }
$abi = if ($Emulator) { 'x86_64' } else { 'arm64-v8a' }
$cargoKey = $rustTarget.ToUpperInvariant().Replace('-', '_')
$ccKey = $rustTarget.Replace('-', '_')
$jniDir = Join-Path $PSScriptRoot "app\src\main\jniLibs\$abi"
$variables = @('ANDROID_HOME','ANDROID_NDK_HOME','CARGO_TARGET_DIR',"CARGO_TARGET_$($cargoKey)_LINKER","CC_$ccKey","CXX_$ccKey","AR_$ccKey",'RUSTFLAGS')
$previous = @{}
foreach ($name in $variables) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
try {
    $env:ANDROID_HOME = $AndroidSdk
    $env:ANDROID_NDK_HOME = $ndk
    $env:CARGO_TARGET_DIR = $targetRoot
    $linker = Join-Path $toolchain "$($rustTarget)29-clang.cmd"
    Set-Item -LiteralPath "Env:CARGO_TARGET_$($cargoKey)_LINKER" -Value $linker
    Set-Item -LiteralPath "Env:CC_$ccKey" -Value $linker
    Set-Item -LiteralPath "Env:CXX_$ccKey" -Value (Join-Path $toolchain "$($rustTarget)29-clang++.cmd")
    Set-Item -LiteralPath "Env:AR_$ccKey" -Value (Join-Path $toolchain 'llvm-ar.exe')
    $env:RUSTFLAGS = (($previous.RUSTFLAGS + ' -C link-arg=-Wl,-z,max-page-size=16384').Trim())
    Push-Location $repo
    try {
        # A one-item array, so splatting passes the flag rather than its characters.
        [string[]]$profileArgs = if ($Release) { '--release' } else { @() }
        cargo "+$($pins.rust)" build --locked -p veil-android --target $rustTarget @profileArgs
        if ($LASTEXITCODE -ne 0) { throw 'Rust Android build failed.' }
        if ($Check) {
            cargo "+$($pins.rust)" clippy --locked -p veil-android -p veil-app -p veil-media --lib --target $rustTarget -- -D warnings
            if ($LASTEXITCODE -ne 0) { throw 'Android Clippy failed.' }
        }
    } finally { Pop-Location }
    New-Item -ItemType Directory -Path $jniDir -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $targetRoot "$rustTarget\$profileDir\libveil_android.so") -Destination $jniDir
    Copy-Item -LiteralPath (Join-Path $ndk "toolchains\llvm\prebuilt\windows-x86_64\sysroot\usr\lib\$rustTarget\libc++_shared.so") -Destination $jniDir
    & (Join-Path $toolchain 'llvm-strip.exe') --strip-debug (Join-Path $jniDir 'libveil_android.so')
    if ($LASTEXITCODE -ne 0) { throw 'Cannot strip Android native library.' }
    Push-Location $PSScriptRoot
    try {
        $versionArgs = @()
        if ($Release) {
            # A release carries the workspace version; versionCode is major*1000000 + minor*1000 + patch.
            $cargo = Get-Content -LiteralPath (Join-Path $repo 'Cargo.toml') -Raw
            if ($cargo -notmatch '(?m)^version = "(\d+)\.(\d+)\.(\d+)"') { throw 'Cannot read the workspace version.' }
            $code = [int]$Matches[1] * 1000000 + [int]$Matches[2] * 1000 + [int]$Matches[3]
            $versionArgs = @("-PveilVersionName=$($Matches[1]).$($Matches[2]).$($Matches[3])", "-PveilVersionCode=$code")
        }
        & $Gradle --no-daemon --console=plain "-PveilAbi=$abi" @versionArgs :app:assemble$variant
        if ($LASTEXITCODE -ne 0) { throw 'APK packaging failed.' }
    } finally { Pop-Location }
    $apk = if ($Release) {
        Join-Path $PSScriptRoot 'app\build\outputs\apk\release\app-release-unsigned.apk'
    } else {
        Join-Path $PSScriptRoot 'app\build\outputs\apk\debug\app-debug.apk'
    }
    $buildTools = Join-Path $AndroidSdk "build-tools\$($pins.buildTools)"
    & (Join-Path $buildTools 'zipalign.exe') -c -P 16 4 $apk
    if ($LASTEXITCODE -ne 0) { throw 'APK page alignment check failed.' }
    if (-not $Release) {
        & (Join-Path $buildTools 'apksigner.bat') verify $apk
        if ($LASTEXITCODE -ne 0) { throw 'APK debug signature check failed.' }
    }
    Push-Location $repo
    try {
        $revision = git rev-parse HEAD
        if ($LASTEXITCODE -ne 0) { throw 'Cannot record source revision.' }
        $dirty = [bool](git status --porcelain --untracked-files=normal)
    } finally { Pop-Location }
    [ordered]@{
        revision = $revision
        dirty = $dirty
        packageId = if ($Release) { 'io.github.jeremy8776.veil' } else { 'io.github.jeremy8776.veil.preview' }
        preview = $pins.preview
        abi = $abi
        toolchain = $pins
        sha256 = (Get-FileHash -LiteralPath $apk -Algorithm SHA256).Hash.ToLowerInvariant()
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path (Split-Path $apk) 'build-info.json') -Encoding utf8
    Write-Output "Built: $apk"
    if ($Install) {
        $adb = Join-Path $AndroidSdk 'platform-tools\adb.exe'
        # -e is the running emulator, -d the USB phone, so the build goes to the device it was made for.
        $device = if ($Emulator) { '-e' } else { '-d' }
        # End the old NativeActivity before replacing its library; retain all app data.
        & $adb $device shell am force-stop io.github.jeremy8776.veil.preview
        if ($LASTEXITCODE -ne 0) { throw 'Cannot stop the existing preview before upgrade.' }
        & $adb $device install -r $apk
        if ($LASTEXITCODE -ne 0) { throw 'Installation failed.' }
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
