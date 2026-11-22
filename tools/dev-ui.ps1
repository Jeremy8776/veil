# Rebuilds and reopens Veil when its interface code changes, for working on the UI.
#
#   powershell -ExecutionPolicy Bypass -File tools\dev-ui.ps1
#   powershell -ExecutionPolicy Bypass -File tools\dev-ui.ps1 -Screen prejoin
#   powershell -ExecutionPolicy Bypass -File tools\dev-ui.ps1 -Screen home -Profile dev
#
# -Screen picks where Veil opens: call (the demo call, the default), prejoin or home.
# -Once builds, opens, checks Veil stays up, closes it and exits.
# Press Ctrl+C to stop. The copy this script opened closes with it.
#
# Invariants:
# - One build at a time, limited to veil-app and eight jobs. Saves are grouped first.
# - A failed build leaves the open copy running, so the last good version stays visible.
# - Veil runs from a copy in target\dev-ui, so a running app never blocks the next build.
# - Only the copy this script started is ever closed.
# - Reopening ends the demo or any call in that copy. Use a normal build to test real calls.

param(
    [ValidateSet('call', 'prejoin', 'home')]
    [string]$Screen = 'call',
    [ValidateSet('release', 'dev')]
    [string]$Profile = 'release',
    [string]$TargetDir = $env:CARGO_TARGET_DIR,
    [switch]$Once
)

$root = Split-Path -Parent $PSScriptRoot
if (-not $TargetDir) { $TargetDir = Join-Path $root 'target' }
$TargetDir = [IO.Path]::GetFullPath($TargetDir)
$profileDir = if ($Profile -eq 'dev') { 'debug' } else { 'release' }
$built = Join-Path $TargetDir "$profileDir\veil-app.exe"
$parked = Join-Path $TargetDir "$profileDir\veil-app.old.exe"
$runDir = Join-Path $root 'target\dev-ui'
$runExe = Join-Path $runDir 'veil-app.exe'
$watched = @('crates\veil-app\src', 'crates\veil-app\assets', 'crates\veil-media\src') |
    ForEach-Object { Join-Path $root $_ }
$script:running = $null

function Get-Stamp {
    $files = Get-ChildItem -Path $watched -Recurse -File -ErrorAction SilentlyContinue
    $total = [int64]0
    foreach ($file in $files) {
        $total = $total -bxor $file.LastWriteTimeUtc.Ticks -bxor $file.Length
    }
    "$(@($files).Count):$total"
}

function Wait-ForQuiet {
    $stamp = Get-Stamp
    while ($true) {
        Start-Sleep -Milliseconds 700
        $now = Get-Stamp
        if ($now -eq $stamp) { return $now }
        $stamp = $now
    }
}

function Stop-Veil {
    if (-not $script:running) { return }
    $process = $script:running
    $script:running = $null
    if ($process.HasExited) { return }
    [void]$process.CloseMainWindow()
    if (-not $process.WaitForExit(2000)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        [void]$process.WaitForExit(2000)
    }
}

function Invoke-Build {
    # A Veil opened from target\release locks its file. A locked file can still be renamed.
    Remove-Item -LiteralPath $parked -Force -ErrorAction SilentlyContinue
    if ((Test-Path -LiteralPath $built) -and -not (Test-Path -LiteralPath $parked)) {
        Move-Item -LiteralPath $built -Destination $parked -ErrorAction SilentlyContinue
    }
    Push-Location $root
    try {
        & cargo build --locked --profile $Profile --target-dir $TargetDir -j 8 -p veil-app --features dev-ui
        return ($LASTEXITCODE -eq 0)
    }
    finally {
        Pop-Location
    }
}

function Start-Veil {
    Stop-Veil
    New-Item -ItemType Directory -Force -Path $runDir | Out-Null
    Copy-Item -LiteralPath $built -Destination $runExe -Force
    $env:VEIL_START = $Screen
    $script:running = Start-Process -FilePath $runExe -WorkingDirectory $root -PassThru
}

function Update-Veil {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    Write-Host "Building..." -ForegroundColor Cyan
    if (Invoke-Build) {
        Start-Veil
        Write-Host ("Opened after {0:N0} s. Watching for changes." -f $clock.Elapsed.TotalSeconds) -ForegroundColor Green
        return $true
    }
    Write-Host "Build failed. The open copy is unchanged. Watching for changes." -ForegroundColor Yellow
    return $false
}

try {
    $ok = Update-Veil
    if ($Once) {
        if (-not $ok) { exit 1 }
        Start-Sleep -Seconds 3
        if ($script:running.HasExited) {
            Write-Host "Veil closed by itself. See veil.log." -ForegroundColor Red
            exit 1
        }
        Write-Host "Veil stayed open." -ForegroundColor Green
        exit 0
    }
    $seen = Get-Stamp
    while ($true) {
        Start-Sleep -Milliseconds 500
        if ((Get-Stamp) -eq $seen) { continue }
        $seen = Wait-ForQuiet
        [void](Update-Veil)
        # Saves made during the build are picked up on the next pass.
        if ((Get-Stamp) -ne $seen) { $seen = '' }
    }
}
finally {
    Stop-Veil
}
