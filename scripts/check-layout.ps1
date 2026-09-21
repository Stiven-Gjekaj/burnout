# Check a VHD of the Windows layout with the tools of Windows.
#
# Windows mounts the VHD read-only, chkdsk checks partition 1, and each file
# is read back through the FAT driver of Windows. Each file has to match the
# manifest that the layout example wrote, and the volume has to hold no file
# that the manifest does not name. Read-only also keeps Windows from adding
# System Volume Information to the volume.
#
# Usage: pwsh scripts/check-layout.ps1 <image.vhd> <manifest>
#
# Mount-DiskImage needs an Administrator.

param(
    [Parameter(Mandatory)] [string] $Image,
    [Parameter(Mandatory)] [string] $Manifest
)

$ErrorActionPreference = 'Stop'
$vhd = (Resolve-Path -LiteralPath $Image).Path
$lines = @(Get-Content -LiteralPath $Manifest -Encoding utf8)

$disk = Mount-DiskImage -ImagePath $vhd -Access ReadOnly -PassThru | Get-Disk
try {
    $part = Get-Partition -DiskNumber $disk.Number -PartitionNumber 1
    if (-not $part.DriveLetter) {
        $part | Add-PartitionAccessPath -AssignDriveLetter
        $part = Get-Partition -DiskNumber $disk.Number -PartitionNumber 1
    }
    $root = "$($part.DriveLetter):"

    Write-Output "==> chkdsk $root"
    chkdsk $root
    if ($LASTEXITCODE -ne 0) {
        throw "chkdsk reports a fault on $root, exit code $LASTEXITCODE"
    }

    Write-Output '==> the hash of each file'
    foreach ($line in $lines) {
        $want, $path = $line -split '  ', 2
        $file = Join-Path $root ($path -replace '/', '\')
        $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash
        if ($got -ne $want) {
            throw "$path holds $got and the manifest gives $want"
        }
    }

    $count = @(Get-ChildItem -LiteralPath "$root\" -Recurse -File -Force).Count
    if ($count -ne $lines.Count) {
        throw "the volume holds $count files and the manifest names $($lines.Count)"
    }
    Write-Output "==> all $($lines.Count) files match"
}
finally {
    Dismount-DiskImage -ImagePath $vhd | Out-Null
}
