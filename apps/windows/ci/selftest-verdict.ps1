# The verdict on a Windows app self-test: fails unless selftest.json exists and every failed step is named in -Excuse (step name -> reason).
# The app exits 0 when its window closes early, so the exit code alone says nothing; Origami.exe exits with the number of failed steps.
param([Parameter(Mandatory)][string]$Out, [Parameter(Mandatory)][int]$ExitCode, [hashtable]$Excuse = @{})
$json = Join-Path $Out "selftest.json"
if (-not (Test-Path $json)) { "FAIL: no selftest.json written (exit code $ExitCode); the app ended before it finished"; exit 1 }
$failed = @((Get-Content -Raw $json | ConvertFrom-Json).steps | Where-Object { -not $_.Ok })
foreach ($s in $failed | Where-Object { $Excuse.ContainsKey($_.Name) }) { "excused: $($s.Name): $($Excuse[$s.Name])" }
$bad = @($failed | Where-Object { -not $Excuse.ContainsKey($_.Name) })
if ($bad.Count -gt 0) { "FAIL: $($bad.Count) self-test step(s) failed: $($bad.Name -join '; ')"; exit 1 }
if ($ExitCode -ne $failed.Count) { "FAIL: exit code $ExitCode, but $($failed.Count) step(s) failed"; exit 1 }
"self-test: $($failed.Count) excused failure(s), no others"
