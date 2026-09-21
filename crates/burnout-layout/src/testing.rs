//! What the tests of this crate share.

use burnout_core::{BlockTarget, MemoryTarget, Result, StrictTarget, Window};

use crate::{format_fat32, Fat32Options, MemorySource};

pub const MIB: u64 = 1024 * 1024;

/// The serial number that [`Drive::format`] gives a volume.
pub const SERIAL: u32 = 0xB0B0_CAFE;

/// A drive with one partition at 1 MiB and a mebibyte to spare after it.
///
/// The drive refuses any access that is not whole sectors, as a raw device
/// does, so a test that passes on it passes on a device.
pub struct Drive {
    pub strict: StrictTarget<MemoryTarget>,
    pub length: u64,
}

impl Drive {
    pub fn new(sector: u32, length: u64) -> Self {
        let drive = MemoryTarget::new(length + 2 * MIB, sector).unwrap();
        Drive {
            strict: StrictTarget::new(drive),
            length,
        }
    }

    /// A partition that FAT32 fits at this sector size.
    pub fn usual(sector: u32) -> Self {
        Drive::new(sector, if sector == 512 { 40 * MIB } else { 272 * MIB })
    }

    /// The same, formatted as FAT32.
    pub fn formatted(sector: u32) -> Self {
        let mut d = Drive::usual(sector);
        d.format().unwrap();
        d
    }

    pub fn sector(&self) -> u32 {
        self.strict.logical_sector_size()
    }

    pub fn partition(&mut self) -> Window<&mut StrictTarget<MemoryTarget>> {
        Window::new(&mut self.strict, MIB, self.length).unwrap()
    }

    pub fn format(&mut self) -> Result<()> {
        let first_sector = (MIB / self.sector() as u64) as u32;
        let options = Fat32Options {
            label: "Burnout",
            serial: SERIAL,
            first_sector,
        };
        format_fat32(self.partition(), &options)
    }

    pub fn bytes(&self) -> &[u8] {
        self.strict.inner().contents()
    }

    pub fn partition_bytes(&self) -> &[u8] {
        &self.bytes()[MIB as usize..(MIB + self.length) as usize]
    }
}

/// Bytes from a generator, so a block in the wrong place cannot pass for
/// the right one, and no run of one file turns up in another.
pub fn pattern(length: usize, seed: u8) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ seed as u64;
    (0..length)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// A tree with a little of everything that a Windows image has: files at
/// several depths, a file of no bytes, a long name and an empty directory.
pub fn windows_like() -> MemorySource {
    let mut tree = MemorySource::new();
    tree.add_file("bootmgr", pattern(4000, 1)).unwrap();
    tree.add_file("efi/boot/bootx64.efi", pattern(12_289, 2))
        .unwrap();
    tree.add_file("efi/microsoft/boot/BCD", pattern(3 * 4096, 3))
        .unwrap();
    tree.add_file("sources/boot.wim", pattern(3 * 1024 * 1024 + 7, 4))
        .unwrap();
    tree.add_file("autorun.inf", *b"").unwrap();
    tree.add_file("A long name with spaces.txt", pattern(10, 5))
        .unwrap();
    tree.add_dir("support/logging").unwrap();
    tree
}
