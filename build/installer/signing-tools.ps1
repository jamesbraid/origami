# Fetch SignTool and the Artifact Signing dlib, write the metadata, and set the step output `command`:
# the signtool command line (ending in $f) for Inno. Auth is the workflow's Azure CLI login.
param(
    [Parameter(Mandatory)][string]$Endpoint,
    [Parameter(Mandatory)][string]$Account,
    [Parameter(Mandatory)][string]$Profile,
    [Parameter(Mandatory)][string]$Dir
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if ($Dir -match '\s') { throw "$Dir has a space, which the signtool command line cannot carry" }
New-Item -ItemType Directory -Force $Dir | Out-Null
# Pinned: the dlib needs a SignTool from SDK 10.0.2261.755 or later.
$packages = @{
    'microsoft.windows.sdk.buildtools' = @('10.0.28000.2957', 'B7163FC6F636172726D0985963E4F7F11C41AFE1C7FDFE78F8051877360ED696')
    'microsoft.artifactsigning.client' = @('1.0.151', '35375A4BC4E1102DA729FC3D8D9925F774C3BAAE6B8B7FF6398D948CA9AB0D8A')
}
foreach ($id in $packages.Keys) {
    $v, $sha = $packages[$id]
    $zip = Join-Path $Dir "$id.zip"
    Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/$id/$v/$id.$v.nupkg" -OutFile $zip
    if ((Get-FileHash $zip -Algorithm SHA256).Hash -ne $sha) { throw "$id $v does not match its pinned SHA256" }
    Expand-Archive $zip (Join-Path $Dir $id) -Force
}
$signtool = (Get-ChildItem -Recurse (Join-Path $Dir 'microsoft.windows.sdk.buildtools') -Filter signtool.exe |
    Where-Object { $_.FullName -match '\\x64\\' } | Select-Object -First 1).FullName
$dlib = Join-Path $Dir 'microsoft.artifactsigning.client\bin\x64\Azure.CodeSigning.Dlib.dll'
if (-not $signtool -or -not (Test-Path $dlib)) { throw 'signtool or the dlib is missing from the packages' }
$meta = Join-Path $Dir 'metadata.json'
[ordered]@{
    Endpoint               = $Endpoint
    CodeSigningAccountName = $Account
    CertificateProfileName = $Profile
    ExcludeCredentials     = @('EnvironmentCredential', 'ManagedIdentityCredential', 'WorkloadIdentityCredential',
        'SharedTokenCacheCredential', 'VisualStudioCredential', 'VisualStudioCodeCredential',
        'AzurePowerShellCredential', 'AzureDeveloperCliCredential', 'InteractiveBrowserCredential')
} | ConvertTo-Json | Set-Content $meta
$command = "$signtool sign /v /fd SHA256 /tr http://timestamp.acs.microsoft.com /td SHA256 /dlib $dlib /dmdf $meta " + '$f'
"signtool: $signtool"
Add-Content $env:GITHUB_OUTPUT "command=$command"
