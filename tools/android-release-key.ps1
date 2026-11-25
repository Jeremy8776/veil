<#
.SYNOPSIS
  Makes Veil's Android release key and stores it as the release environment's secrets.

.DESCRIPTION
  Android installs an update only when it is signed with the same key as the copy already
  installed, so this key signs every Veil APK for the life of the app. It is self-made: no
  paid certificate is involved.

  - The keystore (PKCS12, RSA 4096, alias "veil") and a random 32-byte password go to files
    outside the repository (default ~\.veil\android-release.p12 and
    ~\.veil\android-release.password). Neither is ever printed.
  - With -SetSecrets, the four secrets packaging/android/package.ps1 reads are set on the
    `release` environment with gh, straight from those files: ANDROID_KEYSTORE (base64),
    ANDROID_KEYSTORE_PASSWORD, ANDROID_KEY_ALIAS and ANDROID_KEY_PASSWORD.
  - Keep an offline copy of both files (a password manager holds both). Losing them means
    every phone has to uninstall Veil before it can install a newer one.

  Refuses to overwrite an existing key. Needs JDK 17 (keytool) and, for -SetSecrets, gh.

.EXAMPLE
  powershell -File tools/android-release-key.ps1 -SetSecrets
#>
param(
    [string] $Dir = (Join-Path $HOME '.veil'),
    [string] $Repo = 'Jeremy8776/veil',
    [switch] $SetSecrets
)
$ErrorActionPreference = 'Stop'
$keystore = Join-Path $Dir 'android-release.p12'
$passwordFile = Join-Path $Dir 'android-release.password'
$alias = 'veil'

if ((Test-Path -LiteralPath $keystore) -or (Test-Path -LiteralPath $passwordFile)) {
    throw "$keystore or its password already exists; not replacing a release key."
}
$keytool = if ($env:JAVA_HOME) { Join-Path $env:JAVA_HOME 'bin\keytool.exe' } else { 'keytool' }
New-Item -ItemType Directory -Path $Dir -Force | Out-Null

# A random password, never shown: 32 bytes from the system's secure generator, as hex.
$bytes = New-Object byte[] 32
$rng = [Security.Cryptography.RandomNumberGenerator]::Create()
$rng.GetBytes($bytes)
$rng.Dispose()
$password = -join ($bytes | ForEach-Object { $_.ToString('x2') })
[IO.File]::WriteAllText($passwordFile, $password)
# Only you can read the password file.
icacls $passwordFile /inheritance:r /grant:r "$($env:USERNAME):(R,W)" | Out-Null

$env:VEIL_ANDROID_KEY_PASSWORD = $password
try {
    # :env keeps the password off the command line.
    & $keytool -genkeypair -keystore $keystore -storetype PKCS12 -alias $alias `
        -keyalg RSA -keysize 4096 -validity 36500 -dname 'CN=Veil' `
        -storepass:env VEIL_ANDROID_KEY_PASSWORD -keypass:env VEIL_ANDROID_KEY_PASSWORD
    if ($LASTEXITCODE -ne 0) { throw 'keytool failed.' }
} finally {
    Remove-Item Env:VEIL_ANDROID_KEY_PASSWORD -ErrorAction SilentlyContinue
}
icacls $keystore /inheritance:r /grant:r "$($env:USERNAME):(R,W)" | Out-Null
Write-Output "keystore: $keystore"
Write-Output "password: $passwordFile (keep both secret, keep an offline copy)"

if ($SetSecrets) {
    $encoded = Join-Path $Dir 'android-release.p12.base64'
    $aliasFile = Join-Path $Dir 'android-release.alias'
    try {
        [IO.File]::WriteAllText($encoded, [Convert]::ToBase64String([IO.File]::ReadAllBytes($keystore)))
        [IO.File]::WriteAllText($aliasFile, $alias)
        # Each value goes to gh on standard input from its file, with no added line break.
        $secrets = [ordered]@{
            ANDROID_KEYSTORE          = $encoded
            ANDROID_KEYSTORE_PASSWORD = $passwordFile
            ANDROID_KEY_ALIAS         = $aliasFile
            ANDROID_KEY_PASSWORD      = $passwordFile
        }
        foreach ($name in $secrets.Keys) {
            $run = Start-Process -FilePath 'gh' -NoNewWindow -Wait -PassThru `
                -ArgumentList @('secret', 'set', $name, '--env', 'release', '--repo', $Repo) `
                -RedirectStandardInput $secrets[$name]
            if ($run.ExitCode -ne 0) { throw "gh could not set $name." }
            Write-Output "set $name"
        }
    } finally {
        Remove-Item -LiteralPath $encoded, $aliasFile -Force -ErrorAction SilentlyContinue
    }
}
