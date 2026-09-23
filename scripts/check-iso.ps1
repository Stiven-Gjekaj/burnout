# Make ISOs of a tree with IMAPI2, the image maker of Windows, and check
# that the reader of Burnout gives the tree back from each one, byte for
# byte.
#
# Each ISO also has to read as the file system that the reader has to
# choose for it. IMAPI2 makes UDF beside ISO 9660 and Joliet at each
# revision that it writes, and ISO 9660 with Joliet alone. UDF 2.50 keeps
# its tree in a metadata partition, which Burnout refuses by name, so that
# ISO has to give the refusal.
#
# Usage: pwsh scripts/check-iso.ps1 <tree> <iso_tree>

param(
    [Parameter(Mandatory)] [string] $Tree,
    [Parameter(Mandatory)] [string] $Reader
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $Tree).Path
$work = Join-Path ([System.IO.Path]::GetTempPath()) "burnout-iso-$PID"
New-Item -ItemType Directory -Path $work | Out-Null

# IMAPI2 gives the image as a stream of COM, and this copies it into a file.
Add-Type -TypeDefinition @'
using System.IO;
using System.Runtime.InteropServices.ComTypes;
public static class ImageFile {
    public static void Save(object image, string path, int blockSize, int blocks) {
        IStream stream = (IStream)image;
        byte[] block = new byte[blockSize];
        using (FileStream file = File.Create(path)) {
            for (int n = 0; n < blocks; n++) {
                stream.Read(block, blockSize, System.IntPtr.Zero);
                file.Write(block, 0, blockSize);
            }
        }
    }
}
'@

# Make an ISO of the tree. The file systems add up: 1 is ISO 9660, 2 is
# Joliet and 4 is UDF.
function New-Iso([string] $Path, [int] $FileSystems, [int] $Revision) {
    $image = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
    $image.FileSystemsToCreate = $FileSystems
    if ($FileSystems -band 4) {
        $image.UDFRevision = $Revision
    }
    $image.VolumeName = 'SAMPLE'
    $image.Root.AddTree($source, $false)
    $result = $image.CreateResultImage()
    [ImageFile]::Save($result.ImageStream, $Path, $result.BlockSize, $result.TotalBlocks)
}

# Read one ISO and compare it with the tree.
function Test-Iso([string] $Path, [string] $FileSystem) {
    Write-Output "==> $(Split-Path -Leaf $Path): $FileSystem"
    $out = & $Reader compare $Path $source | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "the reader does not give the tree back from $Path"
    }
    Write-Output $out
    if (-not $out.Contains("($FileSystem)")) {
        throw "the reader did not choose $FileSystem for $Path"
    }
}

try {
    foreach ($revision in 0x102, 0x150, 0x200, 0x201) {
        $iso = Join-Path $work ('udf-{0:x3}.iso' -f $revision)
        New-Iso $iso 7 $revision
        Test-Iso $iso 'UDF'
    }
    $iso = Join-Path $work 'joliet.iso'
    New-Iso $iso 3 0
    Test-Iso $iso 'ISO 9660 with Joliet'

    $iso = Join-Path $work 'udf-250.iso'
    New-Iso $iso 7 0x250
    $refusal = & $Reader list $iso 2>&1 | Out-String
    if ($LASTEXITCODE -eq 0 -or -not $refusal.Contains('metadata partition')) {
        throw "the reader did not refuse the metadata partition of UDF 2.50: $refusal"
    }
    # The runner of CI takes the exit code of the last program as the result
    # of the step, and the refusal left its exit code there.
    $global:LASTEXITCODE = 0
    Write-Output "==> udf-250.iso: refused by name"
    Write-Output $refusal
} finally {
    Remove-Item -Recurse -Force -LiteralPath $work
}
