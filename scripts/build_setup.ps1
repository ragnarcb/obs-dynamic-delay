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

# ffmpeg re-encodes the video of a destination with a bitrate of its own. Pinned
# build (gyan.dev, H.264 on NVENC, AMF, Quick Sync and x264), checked by hash.
$ffVersion = "8.0.1"
$ffDir = "$root\target\ffmpeg-$ffVersion"
$ffHash = "e2aaeaa0fdbc397d4794828086424d4aaa2102cef1fb6874f6ffd29c0b88b673"
if (-not (Test-Path "$ffDir\ffmpeg.exe")) {
    New-Item -ItemType Directory -Force $ffDir | Out-Null
    $zip = "$ffDir\ffmpeg.zip"
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest "https://github.com/GyanD/codexffmpeg/releases/download/$ffVersion/ffmpeg-$ffVersion-essentials_build.zip" -OutFile $zip
    if ((Get-FileHash $zip -Algorithm SHA256).Hash.ToLower() -ne $ffHash) { throw "ffmpeg download does not match the pinned hash" }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $z = [IO.Compression.ZipFile]::OpenRead($zip)
    try {
        foreach ($e in $z.Entries) {
            if ($e.FullName -match '/bin/ffmpeg\.exe$') { [IO.Compression.ZipFileExtensions]::ExtractToFile($e, "$ffDir\ffmpeg.exe", $true) }
            elseif ($e.FullName -match '^[^/]+/LICENSE$') { [IO.Compression.ZipFileExtensions]::ExtractToFile($e, "$ffDir\LICENSE", $true) }
        }
    } finally { $z.Dispose() }
    Remove-Item $zip
    if (-not (Test-Path "$ffDir\ffmpeg.exe") -or -not (Test-Path "$ffDir\LICENSE")) { throw "ffmpeg.exe or its license missing from the download" }
}

$iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found (winget install JRSoftware.InnoSetup)" }
$isccArgs = @("/Q", "/DAppVersion=$version", "/DFFmpegDir=$ffDir")
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
