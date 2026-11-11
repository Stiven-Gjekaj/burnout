//! The values of the attributes that each file has: its standard
//! information and its file names.

/// The attributes of a file, as Windows gives them.
pub(crate) mod file_attribute {
    pub const READ_ONLY: u32 = 0x0001;
    pub const HIDDEN: u32 = 0x0002;
    pub const SYSTEM: u32 = 0x0004;
    pub const DIRECTORY: u32 = 0x0010;
    pub const ARCHIVE: u32 = 0x0020;
    pub const NORMAL: u32 = 0x0080;
    pub const REPARSE_POINT: u32 = 0x0400;
    /// In a file name: the file is a directory with an index of names.
    pub const DUP_FILE_NAME_INDEX_PRESENT: u32 = 0x1000_0000;
    /// In a file name: the file has a view index, such as $Secure.
    pub const DUP_VIEW_INDEX_PRESENT: u32 = 0x2000_0000;
}

/// The four times of a file, in 100 ns units since 1601, as Windows counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Times {
    pub creation: u64,
    pub modification: u64,
    /// The time that the record of the file changed.
    pub record_change: u64,
    pub access: u64,
}

impl Times {
    /// The same time for each of the four.
    pub fn all(time: u64) -> Times {
        Times {
            creation: time,
            modification: time,
            record_change: time,
            access: time,
        }
    }

    fn put(&self, b: &mut [u8]) {
        for (i, t) in [
            self.creation,
            self.modification,
            self.record_change,
            self.access,
        ]
        .iter()
        .enumerate()
        {
            b[8 * i..8 * i + 8].copy_from_slice(&t.to_le_bytes());
        }
    }
}

/// The value of $STANDARD_INFORMATION, in the 72 bytes of NTFS 3.
pub(crate) fn standard_information(times: Times, attributes: u32, security_id: u32) -> Vec<u8> {
    let mut b = vec![0u8; 72];
    times.put(&mut b[..32]);
    b[0x20..0x24].copy_from_slice(&attributes.to_le_bytes());
    b[0x34..0x38].copy_from_slice(&security_id.to_le_bytes());
    b
}

/// The namespace of a file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Namespace {
    /// Any name, with case.
    Posix = 0,
    /// A long name, which has a short name beside it.
    Win32 = 1,
    /// A short 8.3 name, beside a long name.
    Dos = 2,
    /// A name that is both, because it fits 8.3.
    Win32AndDos = 3,
}

/// The value of $FILE_NAME.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileName {
    /// The directory that holds the name, as a file reference.
    pub parent: u64,
    pub times: Times,
    pub allocated: u64,
    pub size: u64,
    pub attributes: u32,
    /// The reparse tag of a reparse point, or the size of the extended
    /// attributes.
    pub reparse_or_ea: u32,
    pub name: Vec<u16>,
    pub namespace: Namespace,
}

impl FileName {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut b = vec![0u8; 0x42];
        b[0x00..0x08].copy_from_slice(&self.parent.to_le_bytes());
        self.times.put(&mut b[0x08..0x28]);
        b[0x28..0x30].copy_from_slice(&self.allocated.to_le_bytes());
        b[0x30..0x38].copy_from_slice(&self.size.to_le_bytes());
        b[0x38..0x3C].copy_from_slice(&self.attributes.to_le_bytes());
        b[0x3C..0x40].copy_from_slice(&self.reparse_or_ea.to_le_bytes());
        b[0x40] = self.name.len() as u8;
        b[0x41] = self.namespace as u8;
        for u in &self.name {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_information_has_its_times_attributes_and_security() {
        let times = Times {
            creation: 1,
            modification: 2,
            record_change: 3,
            access: 4,
        };
        let b = standard_information(
            times,
            file_attribute::HIDDEN | file_attribute::SYSTEM,
            0x100,
        );
        assert_eq!(b.len(), 72);
        for (i, t) in [1u64, 2, 3, 4].iter().enumerate() {
            assert_eq!(&b[8 * i..8 * i + 8], &t.to_le_bytes());
        }
        assert_eq!(&b[0x20..0x24], &6u32.to_le_bytes());
        assert_eq!(&b[0x34..0x38], &0x100u32.to_le_bytes());
    }

    #[test]
    fn a_file_name_has_its_parent_sizes_and_name() {
        let name = FileName {
            parent: 0x0005_0000_0000_0005,
            times: Times::all(9),
            allocated: 4096,
            size: 10,
            attributes: file_attribute::ARCHIVE,
            reparse_or_ea: 0,
            name: "a.txt".encode_utf16().collect(),
            namespace: Namespace::Win32AndDos,
        };
        let b = name.to_bytes();
        assert_eq!(b.len(), 0x42 + 10);
        assert_eq!(&b[..8], &0x0005_0000_0000_0005u64.to_le_bytes());
        assert_eq!(&b[0x08..0x10], &9u64.to_le_bytes());
        assert_eq!(&b[0x28..0x30], &4096u64.to_le_bytes());
        assert_eq!(&b[0x30..0x38], &10u64.to_le_bytes());
        assert_eq!(&b[0x38..0x3C], &0x20u32.to_le_bytes());
        assert_eq!((b[0x40], b[0x41]), (5, 3));
        assert_eq!(&b[0x42..0x44], &[b'a', 0]);
    }
}
