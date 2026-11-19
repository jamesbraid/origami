# Check every listed file is signed, then re-pack the tree over the release zip.
param([Parameter(Mandatory)][string]$Archive, [Parameter(Mandatory)][string]$Work, [Parameter(Mandatory)][string]$Files)
$ErrorActionPreference = 'Stop'
foreach ($f in ($Files -split "`n" | Where-Object { $_ })) {
    $s = Get-AuthenticodeSignature $f
    if ($s.Status -ne 'Valid') { throw "$f is not validly signed: $($s.Status)" }
    if (-not $s.TimeStamperCertificate) { throw "$f has no timestamp" }
    "$($s.Status) $f | $($s.SignerCertificate.Subject) | timestamp $($s.TimeStamperCertificate.NotAfter.ToString('s'))"
}
$top = (Get-ChildItem $Work).Name
if (@($top).Count -ne 1) { throw "expected one top-level entry in $Work" }
Remove-Item $Archive
tar.exe -a -cf $Archive -C $Work $top
if ($LASTEXITCODE -ne 0) { throw "could not pack $Archive" }
"signed $Archive"
