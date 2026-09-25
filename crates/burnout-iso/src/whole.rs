//! Refuse an image that ends before its own volume does.
//!
//! A download that stops early leaves a file that starts as the image does
//! and ends too soon. A raw write of it fills the drive and passes the check,
//! because the check compares the drive with the same short file. The primary
//! volume descriptor of ISO 9660 records the size of the volume, so a short
//! file shows before anything is written.
//!
//! Measured on the twelve ISOs of the test set, Linux and Windows: each file
//! is exactly as long as its volume.

use std::io::{Read, Seek, SeekFrom};

use burnout_core::{Error, Result};

/// The sector of the first volume descriptor.
const FIRST: u64 = 16;

/// The size of a sector of a disc.
const SECTOR: u64 = 2048;

/// The most descriptors that the check looks at for the primary one.
const MOST: u64 = 64;

/// The bytes that the ISO 9660 volume of an image holds, or `None` when the
/// image holds no ISO 9660 that this check can read.
pub fn volume_bytes<R: Read + Seek>(reader: &mut R) -> Result<Option<u64>> {
    let length = reader.seek(SeekFrom::End(0))?;
    let mut d = [0u8; SECTOR as usize];
    for number in FIRST..FIRST + MOST {
        if (number + 1) * SECTOR > length {
            return Ok(None);
        }
        reader.seek(SeekFrom::Start(number * SECTOR))?;
        reader.read_exact(&mut d)?;
        if &d[1..6] != b"CD001" {
            return Ok(None);
        }
        match d[0] {
            // The primary descriptor: the count of blocks at byte 80, and
            // the size of a block at byte 128, each in both byte orders.
            1 => {
                let blocks = u32::from_le_bytes([d[80], d[81], d[82], d[83]]);
                let block = u16::from_le_bytes([d[128], d[129]]);
                return Ok(Some(u64::from(blocks) * u64::from(block)));
            }
            255 => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}

/// Refuse an image that ends before the end of its ISO 9660 volume. `name`
/// names the image in the error.
pub fn check_whole<R: Read + Seek>(reader: &mut R, name: &str) -> Result<()> {
    let length = reader.seek(SeekFrom::End(0))?;
    match volume_bytes(reader)? {
        Some(needed) if needed > length => Err(Error::Incomplete {
            path: name.to_string(),
            image_bytes: length,
            needed_bytes: needed,
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{iso9660, Iso, Item};
    use std::io::Cursor;

    fn image() -> Vec<u8> {
        iso9660(
            &[Item::File("a.txt", &[7u8; 5000]), Item::File("b.txt", b"b")],
            Iso::default(),
        )
    }

    #[test]
    fn a_whole_image_holds_its_volume_exactly() {
        let bytes = image();
        let length = bytes.len() as u64;
        let mut reader = Cursor::new(bytes);
        assert_eq!(volume_bytes(&mut reader).unwrap(), Some(length));
        assert!(check_whole(&mut reader, "test.iso").is_ok());
    }

    #[test]
    fn an_image_that_stops_early_is_refused_with_both_lengths() {
        let whole = image();
        let length = whole.len() as u64;
        let cut = whole[..whole.len() - 2048].to_vec();
        match check_whole(&mut Cursor::new(cut), "test.iso") {
            Err(Error::Incomplete {
                path,
                image_bytes,
                needed_bytes,
            }) => {
                assert_eq!(path, "test.iso");
                assert_eq!((image_bytes, needed_bytes), (length - 2048, length));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_image_with_more_after_its_volume_goes_on() {
        // A hybrid image can carry a partition after the volume, and a file
        // can be padded. Neither is a short file.
        let mut bytes = image();
        bytes.extend_from_slice(&[0u8; 4096]);
        assert!(check_whole(&mut Cursor::new(bytes), "test.iso").is_ok());
    }

    #[test]
    fn an_image_with_no_iso_9660_goes_on() {
        // A disk image holds no volume descriptor, and a file too short to
        // hold one says nothing either. The mode check deals with both.
        for bytes in [vec![0u8; 64 * 2048], vec![0u8; 100]] {
            let mut reader = Cursor::new(bytes);
            assert_eq!(volume_bytes(&mut reader).unwrap(), None);
            assert!(check_whole(&mut reader, "disk.img").is_ok());
        }
    }

    #[test]
    fn an_image_cut_before_its_primary_descriptor_goes_on() {
        let cut = image()[..16 * 2048 + 100].to_vec();
        assert!(check_whole(&mut Cursor::new(cut), "test.iso").is_ok());
    }
}
