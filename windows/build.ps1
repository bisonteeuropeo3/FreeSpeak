# Builds Voice Not for Windows:
#
#   rust\target\release\voice-not.exe          the app
#   windows\dist\VoiceNotSetup.exe             the double-click installer
#
# Nothing here is machine specific; run it after any source change.
$ErrorActionPreference = 'Stop'

$windows = $PSScriptRoot
$rust = Join-Path (Split-Path -Parent $windows) 'rust'

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$env:PATH = "$cargoBin;$env:PATH"

# rustc shells out to `dlltool` when it builds a few crates. On the GNU toolchain
# that binary lives in the toolchain's self-contained directory, which is not on
# PATH, so add it for the build.
$sysroot = & (Join-Path $cargoBin 'rustc.exe') --print sysroot
$mingw = Join-Path $sysroot 'lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained'
if (Test-Path $mingw) { $env:PATH = "$mingw;$env:PATH" }

$manifest = Join-Path $rust 'Cargo.toml'

# cmd /c keeps cargo's stderr from turning into a terminating PowerShell error
# while $ErrorActionPreference is Stop.
cmd /c "cargo build --release --manifest-path `"$manifest`" 2>&1"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$exe = Join-Path $rust 'target\release\voice-not.exe'
$stub = Join-Path $rust 'target\release\voice-not-setup.exe'
$setup = Join-Path $windows 'dist\VoiceNotSetup.exe'

# Relinking wipes the resources, so icons and version metadata are injected after
# every build. The stub must be stamped BEFORE the payload is appended, because
# updating resources rewrites the file. Cosmetic: never fail the build over it.
try {
    & (Join-Path $windows 'tools\embed-icon.ps1') -Exe $exe | Out-Null
    & (Join-Path $windows 'tools\embed-icon.ps1') -Exe $stub `
        -Description 'Voice Not setup - installs the dictation app' | Out-Null
} catch {
    Write-Warning "could not embed the icon: $_"
}

try {
    & (Join-Path $windows 'tools\make-setup.ps1') -Stub $stub -Payload $exe -Out $setup | Out-Null
} catch {
    Write-Warning "could not package the installer: $_"
    $setup = $null
}

$appSize = [math]::Round((Get-Item $exe).Length / 1KB, 0)
Write-Host ""
Write-Host "app:       $exe ($appSize KB)" -ForegroundColor Green
if ($setup -and (Test-Path $setup)) {
    $setupSize = [math]::Round((Get-Item $setup).Length / 1KB, 0)
    Write-Host "installer: $setup ($setupSize KB, double-click to install)" -ForegroundColor Green
}
