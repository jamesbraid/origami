# Build the installer for an app zip, origami-<version>-windows-<arch>.zip; -SignCommand (ending in $f) signs it.
param(
    [Parameter(Mandatory)][string]$Archive,
    [Parameter(Mandatory)][string]$Out,
    [Parameter(Mandatory)][string]$Work,
    [string]$Iscc = 'iscc.exe',
    [string]$SignCommand = '',
    # Test builds only: another version label, and a file only the older tree has.
    [string]$Version = '',
    [switch]$Stale
)
$ErrorActionPreference = 'Stop'
$name = [IO.Path]::GetFileNameWithoutExtension($Archive)
if ($name -notmatch '^origami-(.+)-windows-(x86_64|arm64)$') { throw "$name is not an app zip name" }
$ver = if ($Version) { $Version } else { $Matches[1] }
$arch = $Matches[2]
$num = if ($ver -match '^v?(\d+)\.(\d+)\.(\d+)') { "$($Matches[1]).$($Matches[2]).$($Matches[3]).0" } else { '0.0.0.0' }

$stage = Join-Path $Work "stage-$arch"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
tar.exe -xf $Archive -C $stage
if ($LASTEXITCODE -ne 0) { throw "could not unpack $Archive" }
$top = @(Get-ChildItem $stage)
if ($top.Count -ne 1) { throw "expected one top-level entry in $Archive" }
$tree = $top[0].FullName
foreach ($p in 'bin\origami.exe', 'app\Origami.exe', 'libexec\origami') {
    if (-not (Test-Path (Join-Path $tree $p))) { throw "$Archive has no $p" }
}
if ($Stale) { Set-Content (Join-Path $tree 'app\stale-from-older.txt') 'only the older build has this' }

New-Item -ItemType Directory -Force $Out | Out-Null
$env:ORIGAMI_VERSION = $ver
$env:ORIGAMI_NUM_VERSION = $num
$env:ORIGAMI_ARCH = $arch
$env:ORIGAMI_STAGE = $tree
$env:ORIGAMI_OUT = (Resolve-Path $Out).Path
$isccArgs = @('/Q')
if ($SignCommand) { $isccArgs += '/DSign', "/Ssigntool=$SignCommand" }
& $Iscc @isccArgs (Join-Path $PSScriptRoot 'origami.iss')
if ($LASTEXITCODE -ne 0) { throw "iscc failed: $LASTEXITCODE" }
Remove-Item -Recurse -Force $stage
$setup = Join-Path $Out "origami-$ver-windows-$arch-setup.exe"
if (-not (Test-Path $setup)) { throw "no $setup" }
$setup
