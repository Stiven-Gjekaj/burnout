//! The boot code that lets a BIOS start a Windows drive.
//!
//! UEFI firmware reads the files on partition 1 and never runs this code. A
//! BIOS runs it: the MBR starts the active partition.
//!
//! Burnout writes this code itself, so it ships no boot code of Microsoft
//! and none under the GPL. The source is `boot/mbr.asm`, which `nasm` builds
//! into `boot/mbr.bin`.

/// The code of the MBR, bytes 0 to 439. The disk signature, the table and
/// the boot signature follow it.
pub const MBR_CODE: &[u8; MBR_CODE_BYTES] = include_bytes!("../boot/mbr.bin");

/// How many bytes of the MBR are code.
pub const MBR_CODE_BYTES: usize = 440;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mbr_code_leaves_the_signature_and_the_table_alone() {
        assert_eq!(MBR_CODE.len(), 440);
        // A jump that starts the code, and not the zeros of an empty MBR.
        assert_ne!(MBR_CODE[0], 0);
    }
}
