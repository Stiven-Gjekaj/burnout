//! The footer of a fixed VHD, which the examples add after an image so that
//! Windows mounts it with `Mount-DiskImage`. The footer is for the checks of
//! the gate only, and the product never writes it.

/// The footer of a fixed VHD for a disk of `size` bytes, as the VHD
/// specification gives it.
pub fn vhd_footer(size: u64) -> [u8; 512] {
    let mut footer = [0u8; 512];
    footer[0..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    // A fixed disk has no header, so the offset of the header is all ones.
    footer[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    // The time stays zero, so two runs give one file.
    footer[28..32].copy_from_slice(b"brnt");
    footer[32..36].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[36..40].copy_from_slice(b"Wi2k");
    footer[40..48].copy_from_slice(&size.to_be_bytes());
    footer[48..56].copy_from_slice(&size.to_be_bytes());
    let (cylinders, heads, sectors) = vhd_geometry(size / 512);
    footer[56..58].copy_from_slice(&cylinders.to_be_bytes());
    footer[58] = heads;
    footer[59] = sectors;
    footer[60..64].copy_from_slice(&2u32.to_be_bytes());
    footer[68..84].copy_from_slice(b"burnout-layout-1");
    let sum: u32 = footer.iter().map(|b| *b as u32).sum();
    footer[64..68].copy_from_slice(&(!sum).to_be_bytes());
    footer
}

/// The geometry that the VHD specification computes from a sector count.
fn vhd_geometry(sectors: u64) -> (u16, u8, u8) {
    let total = sectors.min(65_535 * 16 * 255);
    let (per_track, heads, cylinder_times_heads) = if total >= 65_535 * 16 * 63 {
        (255, 16, total / 255)
    } else {
        let mut per_track = 17;
        let mut cylinder_times_heads = total / per_track;
        let mut heads = cylinder_times_heads.div_ceil(1024).max(4);
        if cylinder_times_heads >= heads * 1024 || heads > 16 {
            per_track = 31;
            heads = 16;
            cylinder_times_heads = total / per_track;
        }
        if cylinder_times_heads >= heads * 1024 {
            per_track = 63;
            heads = 16;
            cylinder_times_heads = total / per_track;
        }
        (per_track, heads, cylinder_times_heads)
    };
    (
        (cylinder_times_heads / heads) as u16,
        heads as u8,
        per_track as u8,
    )
}
