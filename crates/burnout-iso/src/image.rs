//! An image file, read at any byte.

use std::io::{Read, Seek, SeekFrom};

use burnout_core::{Error, Result};

/// The sector of a disc. ISO 9660 and UDF both count in it.
pub(crate) const SECTOR: u64 = 2048;

/// An image, with the name that each error gives for it.
#[derive(Debug)]
pub(crate) struct Image<R> {
    reader: R,
    name: String,
    length: u64,
}

impl<R: Read + Seek> Image<R> {
    /// Wrap a reader, and find how long it is.
    pub(crate) fn new(mut reader: R, name: &str) -> Result<Self> {
        let length = reader.seek(SeekFrom::End(0))?;
        Ok(Image {
            reader,
            name: name.to_string(),
            length,
        })
    }

    /// The bytes that the image holds.
    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// `length` bytes from byte `at`.
    ///
    /// A read past the end of the image is refused, and the error names what
    /// the read was for, so a person sees which part of the image is missing.
    pub(crate) fn read_at(&mut self, at: u64, length: usize, what: &str) -> Result<Vec<u8>> {
        let end = at.checked_add(length as u64);
        if end.map_or(true, |end| end > self.length) {
            return Err(self.fault(format!(
                "the image ends at byte {}, before the end of {what} at byte {}",
                self.length,
                at.saturating_add(length as u64)
            )));
        }
        let mut bytes = vec![0u8; length];
        self.reader.seek(SeekFrom::Start(at))?;
        self.reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// One sector of 2048 bytes.
    pub(crate) fn sector(&mut self, number: u64, what: &str) -> Result<Vec<u8>> {
        self.read_at(number * SECTOR, SECTOR as usize, what)
    }

    /// An error about the content of the image.
    pub(crate) fn fault(&self, detail: impl Into<String>) -> Error {
        Error::Image {
            path: self.name.clone(),
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn image(bytes: Vec<u8>) -> Image<Cursor<Vec<u8>>> {
        Image::new(Cursor::new(bytes), "win11.iso").unwrap()
    }

    #[test]
    fn a_read_gives_the_bytes_at_that_place() {
        let bytes: Vec<u8> = (0..3 * SECTOR as usize).map(|n| (n / 7) as u8).collect();
        let mut i = image(bytes.clone());
        assert_eq!(i.length(), 3 * SECTOR);
        assert_eq!(i.read_at(5000, 10, "a test").unwrap(), bytes[5000..5010]);
        assert_eq!(i.sector(2, "a test").unwrap(), bytes[4096..6144]);
    }

    #[test]
    fn a_read_past_the_end_names_the_image_and_what_it_was_for() {
        let mut i = image(vec![0; 4000]);
        let e = i.sector(1, "the anchor of UDF").unwrap_err();
        assert_eq!(
            e.to_string(),
            "cannot read win11.iso: the image ends at byte 4000, before the end of the anchor of UDF at byte 4096"
        );
        assert!(i.read_at(u64::MAX, 2, "a test").is_err());
    }
}
