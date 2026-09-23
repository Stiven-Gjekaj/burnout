//! The volume descriptors of ISO 9660, from sector 16 on.

use std::io::{Read, Seek};

use burnout_core::Result;

use crate::image::Image;

/// The sector of the first descriptor. The 32 KiB before it are the system
/// area, where a hybrid image keeps its boot table.
const FIRST: u64 = 16;

/// The most descriptors that the reader looks at for a terminator.
const MOST: u64 = 64;

/// What every descriptor of ISO 9660 carries after its type.
const STANDARD_ID: &[u8; 5] = b"CD001";

const BOOT_RECORD: u8 = 0;
const PRIMARY: u8 = 1;
const SUPPLEMENTARY: u8 = 2;
const TERMINATOR: u8 = 255;

/// The escape sequences that mark a supplementary descriptor as Joliet, at
/// its three levels.
const JOLIET: [&[u8; 3]; 3] = [b"%/@", b"%/C", b"%/E"];

/// The size of a logical block that the reader takes.
const BLOCK: u16 = 2048;

/// The root directory of one tree: its first block and its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Root {
    pub extent: u32,
    pub length: u32,
}

/// One tree of the image, as a volume descriptor gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tree {
    pub label: String,
    pub root: Root,
}

/// The trees of an image: the primary one, and the Joliet one when there is
/// one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Descriptors {
    pub primary: Tree,
    pub joliet: Option<Tree>,
    /// Whether an El Torito boot record is there, which a disc needs to start.
    pub boot_record: bool,
}

/// Read the volume descriptors, or `None` when the image holds no ISO 9660.
pub(crate) fn read_descriptors<R: Read + Seek>(
    image: &mut Image<R>,
) -> Result<Option<Descriptors>> {
    if image.length() < (FIRST + 1) * 2048 {
        return Ok(None);
    }
    let first = image.sector(FIRST, "the first volume descriptor")?;
    if &first[1..6] != STANDARD_ID {
        return Ok(None);
    }

    let mut primary = None;
    let mut joliet = None;
    let mut boot_record = false;
    let mut ended = false;
    for number in FIRST..FIRST + MOST {
        let d = image.sector(number, "a volume descriptor")?;
        if &d[1..6] != STANDARD_ID {
            return Err(image.fault(format!(
                "sector {number} is not a volume descriptor of ISO 9660, and no terminator came before it"
            )));
        }
        match d[0] {
            PRIMARY if primary.is_none() => primary = Some(tree(image, &d, number, false)?),
            SUPPLEMENTARY if joliet.is_none() && is_joliet(&d) => {
                joliet = Some(tree(image, &d, number, true)?)
            }
            BOOT_RECORD => boot_record = true,
            TERMINATOR => {
                ended = true;
                break;
            }
            _ => {}
        }
    }
    if !ended {
        return Err(image.fault(format!(
            "its volume descriptors hold no terminator in the first {MOST}"
        )));
    }
    let primary =
        primary.ok_or_else(|| image.fault("it has volume descriptors and no primary one"))?;
    Ok(Some(Descriptors {
        primary,
        joliet,
        boot_record,
    }))
}

/// Whether a supplementary descriptor is Joliet, from its escape sequences.
fn is_joliet(d: &[u8]) -> bool {
    JOLIET.iter().any(|escape| &d[88..91] == *escape)
}

/// The tree that one primary or Joliet descriptor names.
fn tree<R: Read + Seek>(image: &Image<R>, d: &[u8], number: u64, joliet: bool) -> Result<Tree> {
    let block = u16::from_le_bytes([d[128], d[129]]);
    if block != BLOCK {
        return Err(image.fault(format!(
            "the descriptor at sector {number} gives blocks of {block} bytes, and Burnout reads blocks of {BLOCK}"
        )));
    }
    let id = &d[40..72];
    let label = if joliet {
        let units: Vec<u16> = id
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(id).into_owned()
    };
    // The root directory record: its extent and its length, low byte first.
    let record = &d[156..190];
    Ok(Tree {
        label: label.trim_end_matches([' ', '\0']).to_string(),
        root: Root {
            extent: u32::from_le_bytes(record[2..6].try_into().unwrap()),
            length: u32::from_le_bytes(record[10..14].try_into().unwrap()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A descriptor of one type, with blocks of 2048 bytes and a root at
    /// block 20 of 2048 bytes.
    fn descriptor(kind: u8, label: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 2048];
        d[0] = kind;
        d[1..6].copy_from_slice(STANDARD_ID);
        d[6] = 1;
        d[40..72].fill(b' ');
        d[40..40 + label.len()].copy_from_slice(label);
        d[128..130].copy_from_slice(&BLOCK.to_le_bytes());
        d[130..132].copy_from_slice(&BLOCK.to_be_bytes());
        d[156] = 34;
        d[156 + 2..156 + 6].copy_from_slice(&20u32.to_le_bytes());
        d[156 + 10..156 + 14].copy_from_slice(&2048u32.to_le_bytes());
        d[156 + 25] = 0x02;
        d
    }

    fn joliet(escape: &[u8; 3], label: &str) -> Vec<u8> {
        let units: Vec<u8> = label.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        let mut d = descriptor(SUPPLEMENTARY, &units);
        // A Joliet label pads with UTF-16 spaces, not with single bytes.
        for at in (40 + units.len()..72).step_by(2) {
            d[at] = 0;
            d[at + 1] = b' ';
        }
        d[88..91].copy_from_slice(escape);
        d
    }

    fn image(descriptors: &[Vec<u8>]) -> Image<Cursor<Vec<u8>>> {
        let mut bytes = vec![0u8; 16 * 2048];
        for d in descriptors {
            bytes.extend_from_slice(d);
        }
        bytes.resize(bytes.len() + 4 * 2048, 0);
        Image::new(Cursor::new(bytes), "linux.iso").unwrap()
    }

    #[test]
    fn a_primary_descriptor_names_the_tree_and_its_root() {
        let mut i = image(&[descriptor(PRIMARY, b"UBUNTU"), descriptor(TERMINATOR, b"")]);
        let d = read_descriptors(&mut i).unwrap().unwrap();
        assert_eq!(d.primary.label, "UBUNTU");
        assert_eq!(
            d.primary.root,
            Root {
                extent: 20,
                length: 2048
            }
        );
        assert_eq!(d.joliet, None);
        assert!(!d.boot_record);
    }

    #[test]
    fn a_joliet_descriptor_at_each_level_is_found_and_its_label_is_read() {
        for escape in JOLIET {
            let mut i = image(&[
                descriptor(PRIMARY, b"UBUNTU"),
                descriptor(BOOT_RECORD, b""),
                joliet(escape, "Ubuntu 26"),
                descriptor(TERMINATOR, b""),
            ]);
            let d = read_descriptors(&mut i).unwrap().unwrap();
            assert_eq!(d.joliet.unwrap().label, "Ubuntu 26");
            assert!(d.boot_record);
        }
    }

    #[test]
    fn a_supplementary_descriptor_without_the_escapes_is_not_joliet() {
        let mut other = descriptor(SUPPLEMENTARY, b"OTHER");
        other[88..91].copy_from_slice(b"%/X");
        let mut i = image(&[
            descriptor(PRIMARY, b"A"),
            other,
            descriptor(TERMINATOR, b""),
        ]);
        assert_eq!(read_descriptors(&mut i).unwrap().unwrap().joliet, None);
    }

    #[test]
    fn an_image_with_no_descriptor_at_sector_16_holds_no_iso_9660() {
        let mut i = image(&[vec![0u8; 2048]]);
        assert_eq!(read_descriptors(&mut i).unwrap(), None);
        let mut short = Image::new(Cursor::new(vec![0u8; 4096]), "tiny").unwrap();
        assert_eq!(read_descriptors(&mut short).unwrap(), None);
    }

    #[test]
    fn descriptors_that_never_end_are_refused() {
        let many: Vec<Vec<u8>> = (0..70).map(|_| descriptor(BOOT_RECORD, b"")).collect();
        let mut with_primary = vec![descriptor(PRIMARY, b"A")];
        with_primary.extend(many);
        let e = read_descriptors(&mut image(&with_primary)).unwrap_err();
        assert!(
            e.to_string().contains("no terminator in the first 64"),
            "{e}"
        );
    }

    #[test]
    fn a_sector_that_is_not_a_descriptor_before_the_terminator_is_refused() {
        let mut i = image(&[descriptor(PRIMARY, b"A"), vec![0u8; 2048]]);
        let e = read_descriptors(&mut i).unwrap_err();
        assert!(
            e.to_string()
                .contains("sector 17 is not a volume descriptor"),
            "{e}"
        );
    }

    #[test]
    fn descriptors_with_no_primary_one_are_refused() {
        let mut i = image(&[descriptor(BOOT_RECORD, b""), descriptor(TERMINATOR, b"")]);
        let e = read_descriptors(&mut i).unwrap_err();
        assert!(e.to_string().contains("no primary one"), "{e}");
    }

    #[test]
    fn a_block_that_is_not_2048_bytes_is_refused() {
        let mut d = descriptor(PRIMARY, b"A");
        d[128..130].copy_from_slice(&1024u16.to_le_bytes());
        let e = read_descriptors(&mut image(&[d, descriptor(TERMINATOR, b"")])).unwrap_err();
        assert!(e.to_string().contains("blocks of 1024 bytes"), "{e}");
    }
}
