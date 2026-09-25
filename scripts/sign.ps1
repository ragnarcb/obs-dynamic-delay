# Signs a file with Authenticode when a code signing certificate is configured.
# Without one it does nothing (the build still works, the files stay unsigned).
#
#   $env:SIGN_PFX       path to the .pfx certificate file
#   $env:SIGN_PFX_PASS  its password
#   $env:SIGN_TIMESTAMP timestamp server (default: http://timestamp.digicert.com)
#
# A cloud signing service (Certum SimplySign, Azure Artifact Signing...) can
# replace the signtool call below; keep the same parameters.
param([Parameter(Mandatory = $true)][string]$File)

if (-not $env:SIGN_PFX -or -not (Test-Path $env:SIGN_PFX)) {
    Write-Host "sign: no certificate configured, $File stays unsigned"
    exit 0
}
$ts = if ($env:SIGN_TIMESTAMP) { $env:SIGN_TIMESTAMP } else { "http://timestamp.digicert.com" }
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $signtool) { Write-Error "signtool.exe not found (install the Windows SDK)"; exit 1 }
& $signtool.FullName sign /fd sha256 /tr $ts /td sha256 /f $env:SIGN_PFX /p $env:SIGN_PFX_PASS /d "Dynamic Delay for OBS" /du "https://github.com/ragnarcb/obs-dynamic-delay" $File
if ($LASTEXITCODE -ne 0) { Write-Error "signing $File failed"; exit $LASTEXITCODE }
& $signtool.FullName verify /pa $File | Out-Null
Write-Host "sign: $File signed"
