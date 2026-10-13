<#
Tests Slerp's Windows installer (packaging/windows/slerp.iss) the way winget
runs it: a silent install for the current user, the files, the registry
entries, the Start menu entry and the uninstall entry it should leave, then a
silent uninstall and what must be gone afterwards.  Before installing, it
gives .ktx2 another thumbnail handler of the user's own, which uninstalling
must put back.  The release workflow runs it on GitHub's Windows machines;
it works in any Windows 10 or 11 session.

  pwsh tools/test-windows-installer.ps1 -Setup Slerp-Setup-windows-x64.exe [-Keep]

-Keep leaves Slerp installed after the install checks.  Exits non-zero on
the first failed check.
#>
param(
    [Parameter(Mandatory)] [string] $Setup,
    [switch] $Keep
)
$ErrorActionPreference = 'Stop'
$app = Join-Path $env:LOCALAPPDATA 'Programs\Slerp'
$classes = 'HKCU:\Software\Classes'
$thumbnailClsid = '{1F742344-7356-4426-940A-FEDDD3F29297}'
$handlerKey = '{e357fccd-a995-4576-b01f-234630154e96}'
$otherHandler = '{0D2A4E5B-6C7D-4E8F-9A0B-1C2D3E4F5A6B}'
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{2434AC7F-0AC1-4A2E-8035-70EBCE4ED931}_is1'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Slerp.lnk'

function Check([bool] $ok, [string] $what) {
    if (-not $ok) { throw "FAILED: $what" }
    Write-Host "ok: $what"
}
function Default([string] $key) {
    (Get-ItemProperty -LiteralPath $key -ErrorAction SilentlyContinue).'(default)'
}

# 1. Another program's thumbnail handler for .ktx2, as if installed before.
$ktx2 = "$classes\.ktx2\ShellEx\$handlerKey"
New-Item -Path $ktx2 -Force | Out-Null
Set-Item -LiteralPath $ktx2 -Value $otherHandler

# 2. The silent install.
$log = Join-Path ([IO.Path]::GetTempPath()) 'slerp-setup.log'
$p = Start-Process -FilePath $Setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$log`"" -Wait -PassThru
Check ($p.ExitCode -eq 0) "Setup exits with 0 (it gave $($p.ExitCode); log in $log)"
foreach ($f in 'slerp.exe', 'slerpw.exe', 'slerp_shell.dll', 'README.txt', 'LICENSE-MIT', 'THIRD-PARTY.md') {
    Check (Test-Path (Join-Path $app $f)) "$app\$f is installed"
}
$version = (& (Join-Path $app 'slerp.exe') --version | Out-String).Trim()
Check ($LASTEXITCODE -eq 0) "slerp.exe --version runs: $version"
Check ((Default "$classes\CLSID\$thumbnailClsid\InprocServer32") -eq (Join-Path $app 'slerp_shell.dll')) 'the thumbnail handler is registered for this user'
Check ((Default "$classes\.glb\ShellEx\$handlerKey") -eq $thumbnailClsid) 'Slerp makes the thumbnails of .glb'
Check ((Default $ktx2) -eq $thumbnailClsid) 'Slerp replaced the other thumbnail handler of .ktx2'
Check (Test-Path "$classes\.glb\OpenWithProgids") '.glb offers Slerp under Open with'
$link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
Check ((Test-Path $shortcut) -and $link.TargetPath -eq (Join-Path $app 'slerpw.exe')) "the Start menu entry opens slerpw.exe"
$entry = Get-ItemProperty -LiteralPath $uninstallKey -ErrorAction SilentlyContinue
Check ($null -ne $entry) "Installed apps lists Slerp $($entry.DisplayVersion) by $($entry.Publisher)"
if ($Keep) { exit 0 }

# 3. The silent uninstall.  The uninstaller copies itself to %TEMP% and
#    returns at once, so the folder is watched until it is gone.
$uninstaller = $entry.UninstallString.Trim('"')
$p = Start-Process -FilePath $uninstaller -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
Check ($p.ExitCode -eq 0) "the uninstaller exits with 0 (it gave $($p.ExitCode))"
for ($i = 0; $i -lt 60 -and (Test-Path $app); $i++) { Start-Sleep -Seconds 1 }
Check (-not (Test-Path $app)) "$app is gone"
Check (-not (Test-Path "$classes\CLSID\$thumbnailClsid")) 'the thumbnail handler is unregistered'
Check ($null -eq (Default "$classes\.glb\ShellEx\$handlerKey")) '.glb has no Slerp thumbnails'
Check ((Default $ktx2) -eq $otherHandler) 'the other thumbnail handler of .ktx2 is back'
Check (-not (Test-Path $shortcut)) 'the Start menu entry is gone'
Check (-not (Test-Path -LiteralPath $uninstallKey)) 'Installed apps no longer lists Slerp'
Remove-Item -LiteralPath "$classes\.ktx2\ShellEx\$handlerKey" -Force
Write-Host 'all checks passed'
