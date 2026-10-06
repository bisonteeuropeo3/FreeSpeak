# Packages the double-clickable installer:
#
#     windows\dist\FreeSpeakSetup.exe
#
# It is the setup stub with freespeak.exe appended, then an 8-byte length and a
# magic marker. The PE loader ignores trailing bytes, so the result is still a
# normal executable - this is the same trick self-extracting archives use.
param(
    [string]$Stub,
    [string]$Payload,
    [string]$Out
)

$ErrorActionPreference = 'Stop'

$windows = Split-Path -Parent $PSScriptRoot
$rust = Join-Path (Split-Path -Parent $windows) 'rust'

if (-not $Stub)    { $Stub    = Join-Path $rust 'target\release\freespeak-setup.exe' }
if (-not $Payload) { $Payload = Join-Path $rust 'target\release\freespeak.exe' }
if (-not $Out)     { $Out     = Join-Path $windows 'dist\FreeSpeakSetup.exe' }

foreach ($path in @($Stub, $Payload)) {
    if (-not (Test-Path $path)) { throw "missing $path - run cargo build --release first" }
}

$outDir = Split-Path -Parent $Out
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$stubBytes = [System.IO.File]::ReadAllBytes($Stub)
$payloadBytes = [System.IO.File]::ReadAllBytes($Payload)

$stream = New-Object System.IO.MemoryStream
$stream.Write($stubBytes, 0, $stubBytes.Length)
$stream.Write($payloadBytes, 0, $payloadBytes.Length)
$length = [BitConverter]::GetBytes([uint64]$payloadBytes.Length)
$stream.Write($length, 0, $length.Length)
$magic = [System.Text.Encoding]::ASCII.GetBytes('FSSETUP1')
$stream.Write($magic, 0, $magic.Length)

[System.IO.File]::WriteAllBytes($Out, $stream.ToArray())
$stream.Dispose()

$size = [math]::Round((Get-Item $Out).Length / 1KB, 0)
$kb = [math]::Round($payloadBytes.Length / 1KB, 0)
Write-Host "packaged: $Out ($size KB, of which $kb KB is the app)"
