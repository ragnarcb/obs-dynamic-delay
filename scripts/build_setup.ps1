# Builds dist\Dynamic-Delay-Setup.exe: the program, signed when a certificate
# is configured (see scripts\sign.ps1), inside the Inno Setup installer.
#
#   powershell -File scripts\build_setup.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
Write-Host "Dynamic Delay $version"

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& "$PSScriptRoot\sign.ps1" -File "$root\target\release\obs-dynamic-delay.exe"

$iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found (winget install JRSoftware.InnoSetup)" }
$isccArgs = @("/Q", "/DAppVersion=$version")
if ($env:SIGN_PFX) {
    # Inno signs the Setup and its uninstaller through the same script
    $isccArgs += "/DSign"
    $isccArgs += "/Ssigntool=powershell -NoProfile -ExecutionPolicy Bypass -File `"$PSScriptRoot\sign.ps1`" -File `$f"
}
& $iscc @isccArgs "installer\setup.iss"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

python scripts\package_streamdeck.py
Move-Item -Force Dynamic-Delay-StreamDeck.streamDeckPlugin dist\
Push-Location dist
Get-FileHash Dynamic-Delay-Setup.exe, Dynamic-Delay-StreamDeck.streamDeckPlugin -Algorithm SHA256 |
    ForEach-Object { "{0}  {1}" -f $_.Hash.ToLower(), (Split-Path $_.Path -Leaf) } | Set-Content SHA256SUMS.txt -Encoding ascii
Get-Content SHA256SUMS.txt
Pop-Location
