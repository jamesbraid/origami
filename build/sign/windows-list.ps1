# Unpack a release zip and list its unsigned PE files (not Microsoft's own DLLs). Sets the step outputs `root` (the unpacked tree) and `files` (one path per line).
param([Parameter(Mandatory)][string]$Archive, [Parameter(Mandatory)][string]$Work)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Work | Out-Null
tar.exe -xf $Archive -C $Work
if ($LASTEXITCODE -ne 0) { throw "could not unpack $Archive" }
$files = Get-ChildItem -Recurse -File $Work -Include *.exe, *.dll |
    Where-Object { (Get-AuthenticodeSignature $_.FullName).Status -eq 'NotSigned' } |
    ForEach-Object FullName
if (-not $files) { throw "no unsigned executables in $Archive" }
$files | ForEach-Object { "to sign: $_" }
Add-Content $env:GITHUB_OUTPUT "root=$Work"
Add-Content $env:GITHUB_OUTPUT "files<<EOF`n$($files -join "`n")`nEOF"
