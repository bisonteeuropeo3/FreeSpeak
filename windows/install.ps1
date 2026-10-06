# Builds FreeSpeak, installs it to %LOCALAPPDATA%\Programs\freespeak and
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

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\freespeak'
$exe        = Join-Path $installDir 'freespeak.exe'
$runKey     = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$startup    = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Startup'

# Leftovers from earlier names, if this is an upgrade.
#
# The current name must never appear here: this runs *after* the login entry is
# registered, and a registry value name is matched case-insensitively, so listing
# "FreeSpeak" would delete the entry that was just created and leave autostart
# silently broken.
$legacyRunNames = @('groq-dictate', 'grok-dictate', 'voice-not')
$legacyDirs     = @(
    (Join-Path $env:LOCALAPPDATA 'Programs\groq-dictate'),
    (Join-Path $env:LOCALAPPDATA 'Programs\grok-dictate'),
    (Join-Path $env:LOCALAPPDATA 'Programs\voice-not')
)
$legacyLinks    = @(
    (Join-Path $startup 'groq-dictate.lnk'),
    (Join-Path $startup 'grok-dictate.lnk'),
    (Join-Path $startup 'voice-not.lnk')
)
# Start menu entries were named after the app, so they move with it.
$legacyShortcuts = @(
    (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Voice Not.lnk'),
    (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Uninstall Voice Not.lnk')
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
    foreach ($shortcut in $legacyShortcuts) {
        if (Test-Path $shortcut) { Remove-Item $shortcut -Force; Write-Host "removed old start menu entry $(Split-Path $shortcut -Leaf)" }
    }
    foreach ($dir in $legacyDirs) {
        if (Test-Path $dir) { Remove-Item $dir -Recurse -Force; Write-Host "removed old install $dir" }
    }
}

# Both names: an upgrade has the old process running under the old image name.
function Stop-FreeSpeak {
    foreach ($name in @('freespeak', 'voice-not')) {
        Get-Process $name -ErrorAction SilentlyContinue | Stop-Process -Force
    }
}

if ($Uninstall) {
    if (Test-Path $exe) { & $exe --uninstall-autostart | Write-Host }
    Stop-FreeSpeak
    Remove-Legacy
    if (Test-Path $installDir) { Remove-Item $installDir -Recurse -Force; Write-Host "removed $installDir" }
    Write-Host "FreeSpeak uninstalled. Your config and logs in $env:LOCALAPPDATA\freespeak were kept." -ForegroundColor Green
    return
}

& (Join-Path $PSScriptRoot 'build.ps1')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Get-Process freespeak -ErrorAction SilentlyContinue | Stop-Process -Force

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$built = Join-Path (Split-Path -Parent $PSScriptRoot) 'rust\target\release\freespeak.exe'

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
    $old = Join-Path $installDir 'freespeak.old.exe'
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

$config = Join-Path $env:LOCALAPPDATA 'freespeak\config'
if (-not (Test-Path $config) -and -not (Test-Path (Join-Path $env:LOCALAPPDATA 'voice-not\config'))) {
    & $exe --init 2>&1 | Out-Null
}

# A Start-menu entry, because the app has no window of its own and the settings
# are otherwise only reachable by running the exe with --settings.
$link = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\FreeSpeak.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($link)
$shortcut.TargetPath = $exe
$shortcut.Arguments = '--settings'
$shortcut.Description = 'FreeSpeak settings - API key and sound'
$shortcut.IconLocation = $exe
$shortcut.Save()
Write-Host "start menu: $link" -ForegroundColor Green
Write-Host ""
& $exe --help 2>&1 | Select-Object -First 3 | ForEach-Object { Write-Host $_ }
Write-Host ""
Write-Host "Start it with:  & `"$exe`"" -ForegroundColor Green
