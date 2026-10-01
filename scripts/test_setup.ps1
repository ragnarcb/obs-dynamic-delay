# Exercise the real Inno uninstaller in an isolated OBS configuration (Windows).
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$testRoot = Join-Path $tempRoot ("dd-setup-" + [guid]::NewGuid())
$app = Join-Path $testRoot "obs-dynamic-delay"
$obs = Join-Path $testRoot "obs-studio"
$profile = Join-Path $obs "basic\profiles\Main"
$scene = Join-Path $obs "basic\scenes\Main.json"
$iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Install Inno Setup 6 before running this test." }

function Assert-True($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Run-Exe([string]$Exe, [string[]]$Arguments) {
    $process = Start-Process -FilePath $Exe -ArgumentList $Arguments -PassThru
    if (-not $process.WaitForExit(60000)) {
        $process.Kill()
        throw "Timed out: $Exe"
    }
    $process.Refresh()
    return $process.ExitCode
}
function Uninstall {
    Run-Exe "$app\unins000.exe" @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', ('/LOG="' + $testRoot + '\uninstall.log"'))
}
function Assert-RecoveryKept {
    foreach ($file in @("obs-dynamic-delay.lua", "obs-dynamic-delay.exe", "setup.log", "unins000.exe")) {
        Assert-True (Test-Path "$app\$file") "Uninstall deleted $file after a failed restoration"
    }
    Assert-True (Test-Path "$profile\service.json.dd-backup") "Original service backup lost"
    Assert-True (Test-Path "$profile\basic.ini.dd-changes.json") "Change journal lost"
}

$oldObsDir = $env:DD_OBS_CONFIG_DIR
$oldInstallDir = $env:DD_INSTALL_DIR
$oldSkip = $env:DD_SKIP_OBS_CHECK
$fakeObs = $null
try {
    New-Item -ItemType Directory -Force $profile, (Split-Path $scene), "$testRoot\dist" | Out-Null
    $env:DD_OBS_CONFIG_DIR = $obs
    $env:DD_INSTALL_DIR = $app
    Remove-Item Env:DD_SKIP_OBS_CHECK -ErrorAction SilentlyContinue
    $service = '{"type":"rtmp_common","settings":{"service":"Twitch","server":"auto","key":"test-original"}}'
    [IO.File]::WriteAllText("$profile\service.json", $service)
    [IO.File]::WriteAllText("$profile\basic.ini", "[Output]`r`nMode=Simple`r`nDelayEnable=true`r`n")
    [IO.File]::WriteAllText("$obs\user.ini", "[Basic]`r`nProfileDir=Main`r`n")
    [IO.File]::WriteAllText($scene, '{"name":"Main","modules":{}}')
    & $iscc /Q "/DAppVersion=0.13.0" "/DTestRoot=$testRoot" "/O$testRoot\dist" "$root\installer\setup.iss"
    Assert-True ($LASTEXITCODE -eq 0) "Inno compilation failed"
    $code = Run-Exe "$testRoot\dist\Dynamic-Delay-Setup.exe" @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
    Assert-True ($code -eq 0) "Setup failed ($code)"
    Assert-RecoveryKept

    # A real process with OBS' executable name exercises both process checks.
    [IO.File]::WriteAllText("$testRoot\fake.cs", 'class FakeObs { static void Main() { System.Threading.Thread.Sleep(120000); } }')
    & "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe" /nologo /target:exe "/out:$testRoot\obs64.exe" "$testRoot\fake.cs"
    Assert-True ($LASTEXITCODE -eq 0) "Could not build simulated OBS process"
    $fakeObs = Start-Process "$testRoot\obs64.exe" -PassThru
    Start-Sleep -Milliseconds 300
    Assert-True (-not $fakeObs.HasExited) "Simulated OBS did not start"
    $before = [IO.File]::ReadAllText($scene)
    Assert-True ((Uninstall) -ne 0) "Uninstall should stop while OBS is open"
    Assert-RecoveryKept
    Assert-True ([IO.File]::ReadAllText($scene) -eq $before) "OBS settings changed while OBS was open"
    Stop-Process -Id $fakeObs.Id -Force
    $fakeObs.WaitForExit()
    $fakeObs = $null

    # Failure to START the helper must also prevent file deletion.
    Move-Item "$app\obs-dynamic-delay.exe" "$app\helper-kept.exe"
    Assert-True ((Uninstall) -ne 0) "Uninstall should stop when the helper is missing"
    Assert-True (Test-Path "$app\obs-dynamic-delay.lua") "Script deleted with missing helper"
    Move-Item "$app\helper-kept.exe" "$app\obs-dynamic-delay.exe"
    Assert-RecoveryKept

    # A nonzero helper exit (bad backup) is different from an Exec failure.
    [IO.File]::WriteAllText("$profile\service.json.dd-backup", 'broken JSON')
    Assert-True ((Uninstall) -ne 0) "Uninstall should stop when restoration fails"
    Assert-RecoveryKept
    Assert-True ([IO.File]::ReadAllText("$app\setup.log") -match 'ERROR:|ERRO:') "Restoration failure was not logged"

    # Explicit helper force accepts incomplete recovery without consuming evidence.
    $legacy = [IO.File]::ReadAllText("$app\obs-service-backup.json")
    Assert-True ((Run-Exe "$app\obs-dynamic-delay.exe" @('--uninstall', '--quiet', '--force')) -eq 0) "Explicit force failed"
    Assert-RecoveryKept
    Assert-True ([IO.File]::ReadAllText("$profile\service.json.dd-backup") -eq 'broken JSON') "Force changed recovery evidence"
    Assert-True ([IO.File]::ReadAllText("$app\obs-service-backup.json") -eq $legacy) "Force changed legacy backup"
    Assert-True ((Uninstall) -ne 0) "Silent Inno uninstall must still require consent"

    # Fix the failed input and retry. Partial restoration must be safe to repeat.
    [IO.File]::WriteAllText("$profile\service.json.dd-backup", $service)
    Assert-True ((Uninstall) -eq 0) "Uninstall retry should succeed"
    Assert-True (-not (Test-Path "$app\obs-dynamic-delay.lua")) "Script was not removed"
    Assert-True (-not (Test-Path "$app\obs-dynamic-delay.exe")) "Executable was not removed"
    $restored = Get-Content "$profile\service.json" -Raw | ConvertFrom-Json
    Assert-True ($restored.settings.key -eq 'test-original') "Wrong service restored"
    Assert-True ([IO.File]::ReadAllText("$profile\basic.ini") -match 'DelayEnable=true') "Stream Delay not restored"
    Assert-True (-not ([IO.File]::ReadAllText($scene) -match 'obs-dynamic-delay.lua')) "Dangling Lua reference"
    Write-Host 'OK: open OBS, missing helper, failed restore and successful retry'
} finally {
    if ($fakeObs -and -not $fakeObs.HasExited) { Stop-Process -Id $fakeObs.Id -Force }
    $env:DD_OBS_CONFIG_DIR = $oldObsDir
    $env:DD_INSTALL_DIR = $oldInstallDir
    $env:DD_SKIP_OBS_CHECK = $oldSkip
    # Preserve the fixture and logs on failure for CI diagnosis.
    Write-Host "Setup test files: $testRoot"
}
