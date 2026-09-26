# Make an ISO of a tree with IMAPI2, the image maker of Windows.
#
# The file systems add up: 1 is ISO 9660, 2 is Joliet and 4 is UDF. The
# revision of UDF is 0x102, 0x150, 0x200, 0x201 or 0x250.
#
# Usage: pwsh scripts/make-iso.ps1 <tree> <iso> [<file systems>] [<revision of UDF>]

param(
    [Parameter(Mandatory)] [string] $Tree,
    [Parameter(Mandatory)] [string] $Iso,
    [int] $FileSystems = 7,
    [int] $Revision = 0x102
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $Tree).Path
$target = [System.IO.Path]::GetFullPath($Iso)

# IMAPI2 gives the image as a stream of COM, and this copies it into a file.
if (-not ('ImageFile' -as [type])) {
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
}

$image = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
# The default limit of IMAPI is the size of a CD, and a Windows ISO is larger.
# Measured: a tree with a 5.3 GB install.esd stops at boot.wim with
# IMAPI_E_IMAGE_SIZE_LIMIT, 0xC0AAB120.
$image.FreeMediaBlocks = [int]::MaxValue
$image.FileSystemsToCreate = $FileSystems
if ($FileSystems -band 4) {
    $image.UDFRevision = $Revision
}
$image.VolumeName = 'SAMPLE'
$image.Root.AddTree($source, $false)
$result = $image.CreateResultImage()
[ImageFile]::Save($result.ImageStream, $target, $result.BlockSize, $result.TotalBlocks)
