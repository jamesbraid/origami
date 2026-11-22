# Install, upgrade and uninstall the installer on this machine and check what each leaves behind.
# -Old is an older build of the same installer (build.ps1 -Version ... -Stale).
param(
    [Parameter(Mandatory)][string]$Setup,
    [Parameter(Mandatory)][string]$Old,
    [Parameter(Mandatory)][string]$Work,
    [switch]$ExpectSigned,
    # The arm64 runner's desktop never gives the app focus, as app-windows-arm64 in build.yml notes.
    [switch]$NoFocus,
    [int]$MinSteps = 40
)
$ErrorActionPreference = 'Stop'
$appId = '{1EC46053-31E6-486A-8AE3-C2DE12CE825F}_is1'
$uninstKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$appId"
$uninstRoot = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
$default = Join-Path $env:LOCALAPPDATA 'Programs\Origami'
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Origami.lnk'
$library = Join-Path $env:LOCALAPPDATA 'Origami\Machines'
New-Item -ItemType Directory -Force $Work | Out-Null

function Check([bool]$ok, [string]$what) {
    if (-not $ok) { throw "FAILED: $what" }
    "ok: $what"
}
function Run-Setup([string]$exe, [string[]]$more = @(), [string]$tag) {
    $log = Join-Path $Work "$tag.log"
    $a = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$log") + $more
    $p = Start-Process $exe -ArgumentList $a -Wait -PassThru
    if ($p.ExitCode -ne 0) { Get-Content $log -Tail 40; throw "$tag exited $($p.ExitCode)" }
}
function Uninstall([string]$dir) {
    $p = Start-Process (Join-Path $dir 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    # The uninstaller hands off to a temporary copy, so wait for the folder to go.
    for ($i = 0; $i -lt 60 -and (Test-Path $dir); $i++) { Start-Sleep 1 }
    Check (-not (Test-Path $dir)) "uninstall removed $dir"
}
function Path-Count([string]$dir) {
    $u = [Environment]::GetEnvironmentVariable('Path', 'User')
    @($u -split ';' | Where-Object { $_.TrimEnd('\') -ieq "$dir\bin" }).Count
}
function Entries { @(Get-ChildItem $uninstRoot | Where-Object { (Get-ItemProperty $_.PSPath).DisplayName -eq 'Origami' }) }
function Check-Signed([string]$file) {
    if (-not $ExpectSigned) { return }
    $s = Get-AuthenticodeSignature $file
    Check ($s.Status -eq 'Valid' -and $null -ne $s.TimeStamperCertificate) "$file is signed and timestamped ($($s.SignerCertificate.Subject))"
    Check ($s.SignerCertificate.Subject -match 'James Braid') "$file is signed by James Braid"
}
function Fresh-Origami([string]$dir) {
    # A new shell reads Path from the registry; build that environment for a child process.
    $path = [Environment]::ExpandEnvironmentVariables(([Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')))
    $psi = [Diagnostics.ProcessStartInfo]::new('cmd.exe', '/c origami --version')
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.Environment['Path'] = $path
    $p = [Diagnostics.Process]::Start($psi)
    $text = $p.StandardOutput.ReadToEnd()
    $p.WaitForExit()
    $text
    Check ($p.ExitCode -eq 0 -and $text -match '^origami ') "origami --version from a fresh shell: $($text.Split("`n")[0])"
    $full = & "$dir\bin\origami.exe" --version
    Check ($LASTEXITCODE -eq 0 -and "$full" -match 'origami ') "origami --version by full path"
}
function Selftest([string]$dir) {
    $out = Join-Path $Work 'selftest-out'
    Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $out | Out-Null
    $app = Join-Path $dir 'app'
    $p = Start-Process -FilePath "$app\Origami.exe" -ArgumentList '--selftest', "`"$out\selftest.json`"", '--settings', "`"$out\settings.json`"", '--stderr', "`"$out\stderr.log`"", '--library', "`"$out\library`"", '--create-product', 'octane2' -WorkingDirectory $app -PassThru
    $null = $p.Handle
    if (-not $p.WaitForExit(240000)) { $p.Kill(); throw 'self-test did not exit' }
    foreach ($f in 'app.log', 'stderr.log') { if (Test-Path "$out\$f") { "--- $f"; Get-Content "$out\$f" -Tail 60 } }
    # The .partial file is a run that did not finish; it must not pass.
    Check (Test-Path "$out\selftest.json") 'the self-test wrote its final report'
    $steps = (Get-Content -Raw "$out\selftest.json" | ConvertFrom-Json).steps
    "self-test steps: $(@($steps).Count)"
    Check (@($steps).Count -ge $MinSteps) "the self-test ran at least $MinSteps steps"
    $skip = if ($NoFocus) { '^(window is the foreground window|about: (the right arrow|typing))' } else { '^$' }
    $failed = @($steps | Where-Object { -not $_.Ok -and $_.Name -notmatch $skip })
    Check ($failed.Count -eq 0 -and ($NoFocus -or $p.ExitCode -eq 0)) "self-test from the installed app (exit $($p.ExitCode); failed: $($failed.Name -join ', '))"
}

$newVer = [IO.Path]::GetFileName($Setup) -replace '^origami-(.+)-windows-.*$', '$1'
$oldVer = [IO.Path]::GetFileName($Old) -replace '^origami-(.+)-windows-.*$', '$1'
Check ($newVer -ne $oldVer) "the older build ($oldVer) differs from this one ($newVer)"
Check-Signed $Setup
$userEnv = 'HKCU:\Environment'
$origPath = (Get-Item $userEnv).GetValue('Path', $null, 'DoNotExpandEnvironmentNames')
$origKind = if ($null -ne $origPath) { (Get-Item $userEnv).GetValueKind('Path') } else { $null }
# A plain-string Path that already names the install folder unexpanded, as a hand edit might.
Set-ItemProperty $userEnv -Name Path -Type String -Value 'C:\Windows;%LOCALAPPDATA%\Programs\Origami\bin'

"== older build, then this one over it"
Run-Setup $Old -tag old
Check (Test-Path "$default\app\stale-from-older.txt") 'the older build installed its extra file'
Run-Setup $Setup -tag upgrade
Check (-not (Test-Path "$default\app\stale-from-older.txt")) 'the upgrade removed the older build''s extra file'
Check ((Entries).Count -eq 1) 'one Origami entry in Settings > Apps after the upgrade'
$k = Get-ItemProperty $uninstKey
Check ($k.DisplayVersion -eq $newVer) "DisplayVersion is $newVer"
Check ($k.Publisher -eq 'James Braid') 'Publisher is James Braid'
Check ($k.DisplayName -eq 'Origami') 'DisplayName is Origami'
Check ((Path-Count $default) -eq 1) 'bin is on the user PATH exactly once'
Check ((Get-Item $userEnv).GetValueKind('Path') -eq 'String') 'Path kept its string type'
Check (-not ((Get-Item $userEnv).GetValue('Path', $null, 'DoNotExpandEnvironmentNames') -like '*%LOCALAPPDATA%*')) 'the unexpanded entry was replaced, not duplicated'
foreach ($p in 'bin\origami.exe', 'app\Origami.exe', 'libexec\origami', 'unins000.exe') { Check (Test-Path "$default\$p") "$p is installed" }
Check (Test-Path $lnk) 'the Start menu entry exists'
$target = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk).TargetPath
Check ($target -eq "$default\app\Origami.exe") "the Start menu entry points at $target"
Check-Signed "$default\unins000.exe"
Check-Signed "$default\app\Origami.exe"

"== the installed command and app"
Fresh-Origami $default
Selftest $default

"== upgrade with the box unticked, then ticked"
Run-Setup $Setup @('/TASKS=!addtopath') -tag untick
Check ((Path-Count $default) -eq 0) 'an upgrade with the box unticked takes the PATH entry out'
Run-Setup $Setup @('/TASKS=addtopath') -tag retick
Check ((Path-Count $default) -eq 1) 'ticking it again puts the entry back once'

"== uninstall leaves the library alone"
New-Item -ItemType Directory -Force "$library\keep-me" | Out-Null
Set-Content "$library\keep-me\disk.img" 'a machine'
Uninstall $default
Check (Test-Path "$library\keep-me\disk.img") 'the library file is still there'
Check (-not (Test-Path $lnk)) 'the Start menu entry is gone'
Check (-not (Test-Path $uninstKey)) 'the Settings > Apps entry is gone'
Check ((Path-Count $default) -eq 0) 'the PATH entry is gone'

"== a folder with spaces, PATH unticked"
$spaced = Join-Path $Work 'with spaces\Origami'
Run-Setup $Setup @("/DIR=`"$spaced`"", '/TASKS=!addtopath') -tag spaced
Check (Test-Path "$spaced\app\Origami.exe") 'installed into a folder with spaces'
Check ((Path-Count $spaced) -eq 0) 'no PATH entry when the box is unticked'
Uninstall $spaced

"== a folder that already holds someone else's bin"
$shared = Join-Path $Work 'shared'
New-Item -ItemType Directory -Force "$shared\bin" | Out-Null
Set-Content "$shared\bin\x.txt" 'not ours'
Run-Setup $Setup @("/DIR=`"$shared`"", '/TASKS=!addtopath') -tag shared
Check (Test-Path "$shared\app\Origami.exe") 'installed beside the stranger'
Check (Test-Path "$shared\bin\x.txt") 'the stranger''s file survived the install'
Start-Process "$shared\unins000.exe" -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES' -Wait
for ($i = 0; $i -lt 60 -and (Test-Path "$shared\app"); $i++) { Start-Sleep 1 }
Check (Test-Path "$shared\bin\x.txt") 'the stranger''s file survived the uninstall'

if ($null -ne $origPath) { Set-ItemProperty $userEnv -Name Path -Type $origKind -Value $origPath }
else { Remove-ItemProperty $userEnv -Name Path -ErrorAction SilentlyContinue }
Remove-Item -Recurse -Force "$library\keep-me" -ErrorAction SilentlyContinue
"installer checks passed"
