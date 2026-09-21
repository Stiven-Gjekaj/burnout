//! What the tests of this crate share.

use burnout_core::{BlockTarget, MemoryTarget, Result, StrictTarget, Window};

use crate::{format_fat32, Fat32Options};

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
