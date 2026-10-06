# Builds Voice Not, installs it to %LOCALAPPDATA%\Programs\voice-not and
# registers it to start at login.
#
#   .\install.ps1              build, install, autostart
#   .\install.ps1 -NoAutostart install without the login entry
#   .\install.ps1 -Uninstall   remove the login entry and the install dir
[CmdletBinding()]
param(
    [switch]$Uninstall,
    [switch]$NoAutostart
)

$ErrorActionPreference = 'Stop'

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\voice-not'
$exe        = Join-Path $installDir 'voice-not.exe'
$runKey     = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$startup    = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Startup'

# Leftovers from earlier names, if this is an upgrade.
$legacyRunNames = @('groq-dictate', 'grok-dictate', 'Voice Not')
$legacyDirs     = @(
    (Join-Path $env:LOCALAPPDATA 'Programs\groq-dictate'),
    (Join-Path $env:LOCALAPPDATA 'Programs\grok-dictate')
)
$legacyLinks    = @(
    (Join-Path $startup 'groq-dictate.lnk'),
    (Join-Path $startup 'grok-dictate.lnk'),
    (Join-Path $startup 'voice-not.lnk')
)

function Remove-Legacy {
    foreach ($name in $legacyRunNames) {
        if (Get-ItemProperty -Path $runKey -Name $name -ErrorAction SilentlyContinue) {
            Remove-ItemProperty -Path $runKey -Name $name
            Write-Host "removed the old login entry '$name'"
        }
    }
    foreach ($link in $legacyLinks) {
        if (Test-Path $link) { Remove-Item $link -Force; Write-Host "removed old startup shortcut $link" }
    }
    foreach ($dir in $legacyDirs) {
        if (Test-Path $dir) { Remove-Item $dir -Recurse -Force; Write-Host "removed old install $dir" }
    }
}

if ($Uninstall) {
    if (Test-Path $exe) { & $exe --uninstall-autostart | Write-Host }
    Remove-Legacy
    Get-Process voice-not -ErrorAction SilentlyContinue | Stop-Process -Force
    if (Test-Path $installDir) { Remove-Item $installDir -Recurse -Force; Write-Host "removed $installDir" }
    Write-Host "Voice Not uninstalled. Your config and logs in $env:LOCALAPPDATA\voice-not were kept." -ForegroundColor Green
    return
}

& (Join-Path $PSScriptRoot 'build.ps1')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Get-Process voice-not -ErrorAction SilentlyContinue | Stop-Process -Force

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$built = Join-Path (Split-Path -Parent $PSScriptRoot) 'rust\target\release\voice-not.exe'

# A killed process does not release its image file the instant Stop-Process
# returns, so copying straight away fails with "used by another process" and
# leaves the previous version installed. Retry while the lock clears; if it
# never does, move the old file aside - Windows allows that even while running.
$copied = $false
for ($attempt = 0; $attempt -lt 15; $attempt++) {
    try {
        Copy-Item $built $exe -Force -ErrorAction Stop
        $copied = $true
        break
    } catch {
        Start-Sleep -Milliseconds 200
    }
}
if (-not $copied) {
    $old = Join-Path $installDir 'voice-not.old.exe'
    Remove-Item $old -Force -ErrorAction SilentlyContinue
    Move-Item $exe $old -Force
    Copy-Item $built $exe -Force
    Remove-Item $old -Force -ErrorAction SilentlyContinue
    Write-Host "the previous copy was still locked and has been moved aside" -ForegroundColor Yellow
}
Write-Host "installed: $exe" -ForegroundColor Green

Remove-Legacy

# The autostart logic lives in the binary so macOS and Windows share it.
if ($NoAutostart) {
    & $exe --uninstall-autostart | Out-Null
    Write-Host "not registering a login entry (-NoAutostart)"
} else {
    & $exe --install-autostart | Write-Host
}

$config = Join-Path $env:LOCALAPPDATA 'voice-not\config'
if (-not (Test-Path $config)) {
    & $exe --init 2>&1 | Out-Null
}
Write-Host ""
& $exe --help 2>&1 | Select-Object -First 3 | ForEach-Object { Write-Host $_ }
Write-Host ""
Write-Host "Start it with:  & `"$exe`"" -ForegroundColor Green
