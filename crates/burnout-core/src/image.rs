//! What the first sector of an image says about itself.
//!
//! This reads 512 bytes and decides nothing about what to do with them. The
//! choice between raw mode and Windows mode needs a reader for the file
//! system inside the image as well, and `burnout-iso` makes it with this
//! check as its first rule.

/// The size of the sector that carries a master boot record.
pub const BOOT_SECTOR_BYTES: usize = 512;

/// Whether the first sector holds a boot signature and a partition table.
///
/// A Linux ISO is a hybrid image: it carries a master boot record so that the
/// same file starts from a disc and from a USB drive. A byte copy of such a
/// file gives a drive that starts.
///
/// A Windows ISO carries no such record, and a byte copy of one gives a drive
/// that most firmware refuses.
///
/// Two things have to be there. The signature alone is not enough, because a
/// sector of zeros with the last two bytes set would pass, and a table with no
/// entry describes no partition to start from.
pub fn has_boot_table(first_sector: &[u8]) -> bool {
    if first_sector.len() < BOOT_SECTOR_BYTES {
        return false;
    }
    if first_sector[510] != 0x55 || first_sector[511] != 0xAA {
        return false;
    }
    // Four entries of sixteen bytes, starting at 446. Byte four of an entry
    // is the type, and a type of zero means the entry is unused.
    (0..4).any(|entry| first_sector[446 + entry * 16 + 4] != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector() -> Vec<u8> {
        vec![0u8; BOOT_SECTOR_BYTES]
    }

    fn with_signature() -> Vec<u8> {
        let mut bytes = sector();
        bytes[510] = 0x55;
        bytes[511] = 0xAA;
        bytes
    }

    #[test]
    fn a_hybrid_image_holds_a_signature_and_a_partition() {
        let mut bytes = with_signature();
        // One entry of type 0x83, which is what a Linux ISO writes.
        bytes[446 + 4] = 0x83;
        assert!(has_boot_table(&bytes));
    }

    #[test]
    fn a_partition_in_the_last_entry_counts_as_well() {
        let mut bytes = with_signature();
        bytes[446 + 3 * 16 + 4] = 0x0C;
        assert!(has_boot_table(&bytes));
    }

    #[test]
    fn a_signature_with_an_empty_table_is_not_a_boot_table() {
        // A sector of zeros with the last two bytes set would otherwise pass,
        // and it describes no partition to start from.
        assert!(!has_boot_table(&with_signature()));
    }

    #[test]
    fn a_partition_with_no_signature_is_not_a_boot_table() {
        let mut bytes = sector();
        bytes[446 + 4] = 0x83;
        assert!(!has_boot_table(&bytes));
    }

    #[test]
    fn a_short_buffer_answers_no_and_does_not_read_past_its_end() {
        assert!(!has_boot_table(&[]));
        assert!(!has_boot_table(&[0u8; 511]));
    }
}
