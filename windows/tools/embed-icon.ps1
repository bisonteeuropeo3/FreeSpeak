# Injects the icon and version metadata into a built exe.
#
# The GNU toolchain here has no resource compiler (no windres, no rc.exe), so
# this uses the Win32 resource-update API instead: BeginUpdateResource,
# UpdateResource, EndUpdateResource. Called from build.ps1 after every build,
# because relinking would otherwise wipe what we inject.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Icon,
    [string]$ProductName = 'FreeSpeak',
    [string]$Description = 'FreeSpeak - push-to-talk dictation for Windows',
    [string]$Version = '0.1.0.0'
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
if (-not $Icon) { $Icon = Join-Path $root 'assets\freespeak.ico' }

if (-not (Test-Path $Exe)) { throw "exe not found: $Exe" }
if (-not (Test-Path $Icon)) { throw "icon not found: $Icon (run tools\make-icon.ps1)" }

if (-not ('Res' -as [type])) {
    Add-Type -Namespace '' -Name Res -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr BeginUpdateResource(string pFileName, bool bDeleteExistingResources);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool UpdateResource(IntPtr hUpdate, IntPtr lpType, IntPtr lpName, ushort wLanguage, byte[] lpData, uint cbData);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool EndUpdateResource(IntPtr hUpdate, bool fDiscard);
'@
}

$RT_ICON = 3
$RT_GROUP_ICON = 14
$RT_VERSION = 16

# ---------------------------------------------------------------- icon parsing
$ico = [System.IO.File]::ReadAllBytes($Icon)
$count = [BitConverter]::ToUInt16($ico, 4)
$images = @()
for ($i = 0; $i -lt $count; $i++) {
    $entry = 6 + 16 * $i
    $images += [pscustomobject]@{
        Width  = $ico[$entry]
        Height = $ico[$entry + 1]
        Length = [BitConverter]::ToUInt32($ico, $entry + 8)
        Offset = [BitConverter]::ToUInt32($ico, $entry + 12)
    }
}

# GRPICONDIR: the group resource that ties the individual RT_ICON entries
# together. Same shape as the .ico directory, but 14-byte entries holding an ID
# rather than a file offset.
$group = New-Object System.IO.MemoryStream
$gw = New-Object System.IO.BinaryWriter($group)
$gw.Write([uint16]0); $gw.Write([uint16]1); $gw.Write([uint16]$count)
for ($i = 0; $i -lt $count; $i++) {
    $img = $images[$i]
    $gw.Write([byte]$img.Width)
    $gw.Write([byte]$img.Height)
    $gw.Write([byte]0)              # palette colours
    $gw.Write([byte]0)              # reserved
    $gw.Write([uint16]1)            # planes
    $gw.Write([uint16]32)           # bits per pixel
    $gw.Write([uint32]$img.Length)
    $gw.Write([uint16]($i + 1))     # resource id of the matching RT_ICON
}
$gw.Flush()
$groupBytes = $group.ToArray()
$gw.Dispose(); $group.Dispose()

# ------------------------------------------------------------ version resource
function Get-Padded([byte[]]$Bytes) {
    $pad = (4 - ($Bytes.Length % 4)) % 4
    if ($pad -eq 0) { return $Bytes }
    $out = New-Object byte[] ($Bytes.Length + $pad)
    [Array]::Copy($Bytes, $out, $Bytes.Length)
    return $out
}

function Add-Pad([System.IO.MemoryStream]$Stream) {
    while ($Stream.Length % 4 -ne 0) { $Stream.WriteByte(0) }
}

function New-VsNode {
    param(
        [string]$Key,
        [byte[]]$Value = (New-Object byte[] 0),
        [int]$Type = 1,
        [byte[][]]$Children = @(),
        [int]$ValueLength = -1
    )
    if ($ValueLength -lt 0) { $ValueLength = $Value.Length }

    $body = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($body)
    $bw.Write([uint16]0)                    # wLength, patched below
    $bw.Write([uint16]$ValueLength)         # wValueLength
    $bw.Write([uint16]$Type)                # wType: 1 = text, 0 = binary
    $bw.Write([System.Text.Encoding]::Unicode.GetBytes($Key + [char]0))
    $bw.Flush()
    # Padding is measured from the start of the node, not from the end of the
    # key: the 6-byte header makes those differ, and misaligned nodes are
    # silently ignored by Windows.
    Add-Pad $body
    if ($Value.Length -gt 0) {
        $bw.Write($Value)
        $bw.Flush()
        Add-Pad $body
    }
    foreach ($child in $Children) { $bw.Write($child) }
    $bw.Flush()
    $bytes = $body.ToArray()
    $bw.Dispose(); $body.Dispose()

    $length = [uint16]$bytes.Length
    $bytes[0] = [byte]($length -band 0xFF)
    $bytes[1] = [byte](($length -shr 8) -band 0xFF)
    return $bytes
}

function New-VsString([string]$Value) {
    return [System.Text.Encoding]::Unicode.GetBytes($Value + [char]0)
}

$parts = $Version.Split('.')
while ($parts.Count -lt 4) { $parts += '0' }
$v = $parts | ForEach-Object { [int]$_ }

$fixed = New-Object System.IO.MemoryStream
$fw = New-Object System.IO.BinaryWriter($fixed)
$fw.Write([uint32]4277077181)                       # signature 0xFEEF04BD
$fw.Write([uint32]0x00010000)                       # struct version
$fw.Write([uint32](($v[0] -shl 16) -bor $v[1]))     # file version MS
$fw.Write([uint32](($v[2] -shl 16) -bor $v[3]))     # file version LS
$fw.Write([uint32](($v[0] -shl 16) -bor $v[1]))     # product version MS
$fw.Write([uint32](($v[2] -shl 16) -bor $v[3]))     # product version LS
$fw.Write([uint32]0x3F)                             # file flags mask
$fw.Write([uint32]0)                                # file flags
$fw.Write([uint32]0x40004)                          # VOS_NT_WINDOWS32
$fw.Write([uint32]1)                                # VFT_APP
$fw.Write([uint32]0)                                # subtype
$fw.Write([uint32]0); $fw.Write([uint32]0)          # file date
$fw.Flush()
$fixedBytes = $fixed.ToArray()
$fw.Dispose(); $fixed.Dispose()

$strings = @(
    @('CompanyName', $ProductName),
    @('FileDescription', $Description),
    @('FileVersion', $Version),
    @('InternalName', 'freespeak'),
    @('OriginalFilename', 'freespeak.exe'),
    @('ProductName', $ProductName),
    @('ProductVersion', $Version)
)
$stringNodes = foreach ($pair in $strings) {
    New-VsNode -Key $pair[0] -Value (New-VsString $pair[1]) -ValueLength ($pair[1].Length + 1)
}

$stringTable = New-VsNode -Key '040904B0' -Children @($stringNodes)
$stringFileInfo = New-VsNode -Key 'StringFileInfo' -Children @($stringTable)

$translation = New-Object byte[] 4
[BitConverter]::GetBytes([uint16]0x0409).CopyTo($translation, 0)   # US English
[BitConverter]::GetBytes([uint16]1200).CopyTo($translation, 2)     # Unicode
$varNode = New-VsNode -Key 'Translation' -Value $translation -Type 0 -ValueLength 4
$varFileInfo = New-VsNode -Key 'VarFileInfo' -Children @($varNode)

$versionBytes = New-VsNode -Key 'VS_VERSION_INFO' -Value $fixedBytes -Type 0 `
    -ValueLength 52 -Children @($stringFileInfo, $varFileInfo)

# ------------------------------------------------------------------- injection
$handle = [Res]::BeginUpdateResource($Exe, $false)
if ($handle -eq [IntPtr]::Zero) {
    throw "BeginUpdateResource failed (error $([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"
}

try {
    for ($i = 0; $i -lt $count; $i++) {
        $img = $images[$i]
        $slice = New-Object byte[] $img.Length
        [Array]::Copy($ico, [int]$img.Offset, $slice, 0, [int]$img.Length)
        if (-not [Res]::UpdateResource($handle, [IntPtr]$RT_ICON, [IntPtr]($i + 1), 0, $slice, [uint32]$slice.Length)) {
            throw "UpdateResource(RT_ICON $($i + 1)) failed (error $([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"
        }
    }
    if (-not [Res]::UpdateResource($handle, [IntPtr]$RT_GROUP_ICON, [IntPtr]1, 0, $groupBytes, [uint32]$groupBytes.Length)) {
        throw "UpdateResource(RT_GROUP_ICON) failed (error $([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"
    }
    if (-not [Res]::UpdateResource($handle, [IntPtr]$RT_VERSION, [IntPtr]1, 0, $versionBytes, [uint32]$versionBytes.Length)) {
        throw "UpdateResource(RT_VERSION) failed (error $([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"
    }
} catch {
    [void][Res]::EndUpdateResource($handle, $true)   # discard
    throw
}

if (-not [Res]::EndUpdateResource($handle, $false)) {
    throw "EndUpdateResource failed (error $([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"
}

$info = (Get-Item $Exe).VersionInfo
Write-Host "embedded icon ($count sizes) and version $($info.FileVersion) into $(Split-Path $Exe -Leaf)" -ForegroundColor DarkGray
Write-Host "  ProductName     : $($info.ProductName)" -ForegroundColor DarkGray
Write-Host "  FileDescription : $($info.FileDescription)" -ForegroundColor DarkGray
