//! The boot code that lets a BIOS start a Windows drive.
//!
//! UEFI firmware reads the files on partition 1 and never runs this code. A
//! BIOS runs it: the MBR starts the active partition, and the boot sector of
//! the FAT32 volume loads `BOOTMGR` from it, the way the boot sector of
//! Windows does. `BOOTMGR` then reads `boot\BCD`, as it does from the ISO.
//!
//! Burnout writes this code itself, so it ships no boot code of Microsoft
//! and none under the GPL. The sources are `boot/mbr.asm` and
//! `boot/fat32.asm`, which `nasm` builds into the `.bin` files beside them.

use std::io::{self, Read, Seek, SeekFrom, Write};

/// The code of the MBR, bytes 0 to 439. The disk signature, the table and
/// the boot signature follow it.
pub const MBR_CODE: &[u8; MBR_CODE_BYTES] = include_bytes!("../boot/mbr.bin");

/// How many bytes of the MBR are code.
pub const MBR_CODE_BYTES: usize = 440;

/// The boot code of the FAT32 volume: part 1 for sector 0, and part 2 for
/// sector 2.
const FAT32_CODE: &[u8; 1024] = include_bytes!("../boot/fat32.bin");

/// The jump at the start of the boot sector.
const JUMP: std::ops::Range<usize> = 0..3;

/// Where the BPB of a FAT32 boot sector ends and its code starts.
const CODE_START: usize = 0x5A;

/// The sector of the volume that holds part 2. It is a reserved sector that
/// nothing else uses: sector 1 counts the free clusters, and sector 6 holds
/// the copy of the boot sector. The boot sector of Windows keeps code there
/// too.
pub const PART_2_SECTOR: u64 = 2;

/// Where the boot sector keeps the sector that counts the free clusters.
const FS_INFO_SECTOR_AT: u64 = 48;

/// Where the boot sector keeps the sector of its own copy.
const BACKUP_SECTOR_AT: u64 = 50;

/// Where the boot sector keeps how many sectors come before the table.
const RESERVED_SECTORS_AT: u64 = 14;

/// Put the code of part 1 around the BPB of `sector`, and keep the BPB.
pub fn with_fat32_code(sector: &[u8; 512]) -> [u8; 512] {
    let mut out = *sector;
    out[JUMP].copy_from_slice(&FAT32_CODE[JUMP]);
    out[CODE_START..].copy_from_slice(&FAT32_CODE[CODE_START..512]);
    out
}

/// The 512 bytes of part 2, which go at the start of sector 2.
pub fn fat32_part_2() -> &'static [u8] {
    &FAT32_CODE[512..]
}

/// Write the boot code into a FAT32 volume that `fatfs` just formatted: part 1
/// into the boot sector and into its copy, and part 2 into sector 2.
///
/// `volume` is the volume from its first byte, and it takes reads and writes
/// at any byte. Sector 2 must be a reserved sector that the volume uses for
/// nothing else, or the code would land on its data, so the volume is
/// refused otherwise. Whatever sector 2 held goes, and the rest of it after
/// part 2 is zero.
pub fn write_fat32_code<V: Read + Write + Seek>(
    volume: &mut V,
    sector_size: u32,
) -> io::Result<()> {
    let sector_size = sector_size as u64;
    let reserved = read_u16(volume, RESERVED_SECTORS_AT)? as u64;
    let fs_info = read_u16(volume, FS_INFO_SECTOR_AT)? as u64;
    let backup = read_u16(volume, BACKUP_SECTOR_AT)? as u64;
    if reserved <= PART_2_SECTOR || fs_info == PART_2_SECTOR || backup == PART_2_SECTOR {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the volume keeps no free reserved sector for the boot code",
        ));
    }

    for boot_sector in [0, backup * sector_size] {
        let mut sector = [0u8; 512];
        volume.seek(SeekFrom::Start(boot_sector))?;
        volume.read_exact(&mut sector)?;
        volume.seek(SeekFrom::Start(boot_sector))?;
        volume.write_all(&with_fat32_code(&sector))?;
    }
    let mut part_2 = vec![0u8; sector_size as usize];
    part_2[..512].copy_from_slice(fat32_part_2());
    volume.seek(SeekFrom::Start(PART_2_SECTOR * sector_size))?;
    volume.write_all(&part_2)?;
    Ok(())
}

fn read_u16<V: Read + Seek>(volume: &mut V, at: u64) -> io::Result<u16> {
    let mut bytes = [0u8; 2];
    volume.seek(SeekFrom::Start(at))?;
    volume.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn the_mbr_code_leaves_the_signature_and_the_table_alone() {
        assert_eq!(MBR_CODE.len(), 440);
        // A jump that starts the code, and not the zeros of an empty MBR.
        assert_ne!(MBR_CODE[0], 0);
    }

    #[test]
    fn both_parts_end_with_the_boot_signature() {
        assert_eq!(&FAT32_CODE[510..512], &[0x55, 0xAA]);
        assert_eq!(&FAT32_CODE[1022..], &[0x55, 0xAA]);
        // The jump of a FAT32 boot sector goes past the BPB.
        assert_eq!(&FAT32_CODE[..3], &[0xEB, 0x58, 0x90]);
    }

    #[test]
    fn the_bpb_survives_the_code() {
        let mut sector = [0u8; 512];
        for (i, b) in sector.iter_mut().enumerate() {
            *b = i as u8;
        }
        let out = with_fat32_code(&sector);
        assert_eq!(&out[3..CODE_START], &sector[3..CODE_START]);
        assert_eq!(&out[..3], &FAT32_CODE[..3]);
        assert_eq!(&out[CODE_START..], &FAT32_CODE[CODE_START..512]);
    }

    /// A volume of 8 reserved sectors, with the copy of the boot sector in
    /// sector 6, as `fatfs` formats it.
    fn volume(sector: u32) -> Cursor<Vec<u8>> {
        let s = sector as usize;
        let mut bytes = vec![0u8; 16 * s];
        bytes[11..13].copy_from_slice(&(sector as u16).to_le_bytes());
        bytes[14..16].copy_from_slice(&8u16.to_le_bytes());
        bytes[48..50].copy_from_slice(&1u16.to_le_bytes());
        bytes[50..52].copy_from_slice(&6u16.to_le_bytes());
        bytes[510] = 0x55;
        bytes[511] = 0xAA;
        let copy = bytes[..s].to_vec();
        bytes[6 * s..7 * s].copy_from_slice(&copy);
        Cursor::new(bytes)
    }

    #[test]
    fn the_code_goes_into_both_boot_sectors_and_sector_2() {
        for sector in [512u32, 4096] {
            let s = sector as usize;
            let mut v = volume(sector);
            write_fat32_code(&mut v, sector).unwrap();
            let bytes = v.into_inner();
            assert_eq!(&bytes[CODE_START..512], &FAT32_CODE[CODE_START..512]);
            assert_eq!(&bytes[..s], &bytes[6 * s..7 * s], "the copy is the same");
            assert_eq!(&bytes[2 * s..2 * s + 512], fat32_part_2());
            assert!(bytes[2 * s + 512..3 * s].iter().all(|b| *b == 0));
            assert_eq!(
                &bytes[11..13],
                &(sector as u16).to_le_bytes(),
                "the BPB stays"
            );
        }
    }

    #[test]
    fn whatever_sector_2_held_goes() {
        let mut v = volume(512);
        v.get_mut()[2 * 512..3 * 512].fill(0xAA);
        write_fat32_code(&mut v, 512).unwrap();
        assert_eq!(&v.get_ref()[2 * 512..3 * 512], fat32_part_2());
    }

    #[test]
    fn a_volume_that_uses_sector_2_is_refused() {
        // Too few reserved sectors, the free count in sector 2, or the copy
        // of the boot sector in sector 2.
        for (at, value) in [(14, 2u16), (48, 2), (50, 2)] {
            let mut v = volume(512);
            v.get_mut()[at..at + 2].copy_from_slice(&value.to_le_bytes());
            let before = v.get_ref().clone();
            assert!(write_fat32_code(&mut v, 512).is_err());
            assert_eq!(v.get_ref(), &before, "nothing changed");
        }
    }
}
