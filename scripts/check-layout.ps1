# Check a VHD of the Windows layout with the tools of Windows.
#
# Windows mounts the VHD read-only. chkdsk checks partition 1, which is
# FAT32, and partition 2, which is exFAT, and each file is read back through
# the drivers of Windows. Each file has to match the manifest that the layout
# example wrote for its partition, and each volume has to hold no file that
# its manifest does not name. Read-only also keeps Windows from adding
# System Volume Information to a volume.
#
# Usage: pwsh scripts/check-layout.ps1 <image.vhd> <boot-manifest> <install-manifest>
#
# Mount-DiskImage needs an Administrator.

param(
    [Parameter(Mandatory)] [string] $Image,
    [Parameter(Mandatory)] [string] $BootManifest,
    [Parameter(Mandatory)] [string] $InstallManifest
)

$ErrorActionPreference = 'Stop'
$vhd = (Resolve-Path -LiteralPath $Image).Path

# Check one partition of the mounted disk against its manifest.
function Test-Partition([int] $Disk, [int] $Number, [string] $Manifest) {
    $lines = @(Get-Content -LiteralPath $Manifest -Encoding utf8)
    $part = Get-Partition -DiskNumber $Disk -PartitionNumber $Number
    if (-not $part.DriveLetter) {
        $part | Add-PartitionAccessPath -AssignDriveLetter
        $part = Get-Partition -DiskNumber $Disk -PartitionNumber $Number
    }
    $root = "$($part.DriveLetter):"
    $volume = Get-Volume -DriveLetter $part.DriveLetter
    Write-Output "==> partition $Number is $root, $($volume.FileSystem), labelled $($volume.FileSystemLabel)"

    Write-Output "==> chkdsk $root"
    chkdsk $root
    if ($LASTEXITCODE -ne 0) {
        throw "chkdsk reports a fault on $root, exit code $LASTEXITCODE"
    }

    Write-Output "==> the hash of each file of partition $Number"
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
        throw "partition $Number holds $count files and its manifest names $($lines.Count)"
    }
    Write-Output "==> all $($lines.Count) files of partition $Number match"
}

$disk = Mount-DiskImage -ImagePath $vhd -Access ReadOnly -PassThru | Get-Disk
try {
    Test-Partition $disk.Number 1 $BootManifest
    Test-Partition $disk.Number 2 $InstallManifest
}
finally {
    Dismount-DiskImage -ImagePath $vhd | Out-Null
}
