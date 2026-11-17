<#
.SYNOPSIS
  Build, sign and package Veil for Windows x64.

.DESCRIPTION
  Produces, in $env:DIST (default: dist):
    veil-<version>-windows-x64.msi            Veil for all users, with its firewall rule (veil.wxs)
    veil-<version>-windows-x64-portable.zip   veil-app.exe, veil-node.exe, veil-probe.exe, licences

  The binaries link the C runtime statically (+crt-static), so neither needs the Visual C++
  redistributable. Signing is sign.ps1's job: without signing secrets it warns and leaves the
  files unsigned. The approach follows ArtCraft's packaging/windows/package.ps1.

  Needs: Rust (MSVC), the Windows SDK (signtool.exe), and WiX v5:
    dotnet tool install --global wix --version 5.0.2
    wix extension add -g WixToolset.Firewall.wixext/5.0.2

.EXAMPLE
  pwsh packaging/windows/package.ps1
  pwsh packaging/windows/package.ps1 -SkipBuild
#>
param([switch] $SkipBuild)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Target = 'x86_64-pc-windows-msvc'

function Invoke-Native([string] $What, [scriptblock] $Block) {
  Write-Output "==> $What"
  & $Block
  if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

$Version = $env:VEIL_VERSION
if (-not $Version) {
  $meta = cargo metadata --format-version 1 --no-deps --locked --manifest-path (Join-Path $Root 'Cargo.toml') | ConvertFrom-Json
  $Version = ($meta.packages | Where-Object name -eq 'veil-app').version
}
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "'$Version' is not a version like 1.2.3" }
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist' }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
Write-Output "Veil $Version for Windows x64"

if (-not $SkipBuild) {
  # Static CRT. With an explicit --target, RUSTFLAGS reaches only the target's code, not build
  # scripts or proc-macros. RUSTFLAGS wins over every target-specific flag, so append to it.
  $env:RUSTFLAGS = (("$env:RUSTFLAGS" + ' -C target-feature=+crt-static').Trim())
  Invoke-Native "cargo build ($Target)" {
    cargo build --release --locked -p veil-app -p veil-node -p veil-probe --target $Target --manifest-path (Join-Path $Root 'Cargo.toml')
  }
}
$Bin = Join-Path $TargetDir "$Target\release"

# PE headers: machine x64 (0x8664); the app is a window program (subsystem 2, no console
# opens with it), the tools are console programs (3) so their output reaches the terminal.
function Get-PeHeader([string] $Path) {
  $bytes = [IO.File]::ReadAllBytes($Path)
  $pe = [BitConverter]::ToInt32($bytes, 0x3C)
  return @{ Machine = [BitConverter]::ToUInt16($bytes, $pe + 4); Subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C) }
}
foreach ($check in @(@('veil-app.exe', 2), @('veil-node.exe', 3), @('veil-probe.exe', 3))) {
  $h = Get-PeHeader (Join-Path $Bin $check[0])
  if ($h.Machine -ne 0x8664) { throw "$($check[0]) is for machine 0x$('{0:X}' -f $h.Machine), not x64" }
  if ($h.Subsystem -ne $check[1]) { throw "$($check[0]) has PE subsystem $($h.Subsystem), expected $($check[1])" }
  Write-Output "ok $($check[0]): x64, PE subsystem $($h.Subsystem)"
}

$Stage = Join-Path $TargetDir 'windows-package'
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
foreach ($exe in 'veil-app.exe', 'veil-node.exe', 'veil-probe.exe') { Copy-Item (Join-Path $Bin $exe) $Stage }
foreach ($f in 'LICENSE-MIT', 'LICENSE-APACHE', 'README.md', 'docs/DEPENDENCIES.md', 'crates/veil-node/veil-node.example.toml') {
  Copy-Item (Join-Path $Root $f) $Stage
}
[IO.File]::WriteAllText((Join-Path $Stage 'installed-by-msi'), "Installed by Veil's MSI. The app updates itself through a newer MSI.`r`n")

& (Join-Path $PSScriptRoot 'sign.ps1') (Join-Path $Stage 'veil-app.exe') (Join-Path $Stage 'veil-node.exe') (Join-Path $Stage 'veil-probe.exe')

# The icon for the Start menu and Installed apps: an .ico holding the 256 px PNG as it is
# (Windows Vista and later read PNG entries).
$Png = [IO.File]::ReadAllBytes((Join-Path $Root 'crates\veil-app\assets\veil-256.png'))
$Ico = Join-Path $Stage 'veil.ico'
$stream = [IO.MemoryStream]::new()
$w = [IO.BinaryWriter]::new($stream)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]1)          # reserved, icon, 1 image
$w.Write([byte]0); $w.Write([byte]0); $w.Write([byte]0); $w.Write([byte]0) # 256x256, no palette
$w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$Png.Length); $w.Write([uint32]22)
$w.Write($Png); $w.Flush()
[IO.File]::WriteAllBytes($Ico, $stream.ToArray())

# ---- MSI ---------------------------------------------------------------------------------------
$Msi = Join-Path $Dist "veil-$Version-windows-x64.msi"
Invoke-Native 'wix build' {
  wix build (Join-Path $PSScriptRoot 'veil.wxs') -arch x64 -ext WixToolset.Firewall.wixext `
    -d "Version=$Version" -d "BinDir=$Stage" -d "IconPath=$Ico" -o $Msi
}
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($Msi, '.wixpdb'))
& (Join-Path $PSScriptRoot 'sign.ps1') $Msi

# ---- portable zip ------------------------------------------------------------------------------
# No installed-by-msi in the zip: a portable copy is updated by hand.
$Name = "veil-$Version-windows-x64-portable"
$Portable = Join-Path $TargetDir "windows-portable\$Name"
Remove-Item -Recurse -Force $Portable -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Portable | Out-Null
Get-ChildItem $Stage -File | Where-Object { $_.Name -notin 'installed-by-msi', 'veil.ico' } | Copy-Item -Destination $Portable
$Zip = Join-Path $Dist "$Name.zip"
Remove-Item -Force $Zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$Portable\*" -DestinationPath $Zip

Get-Item $Msi, $Zip | Format-Table Name, Length
