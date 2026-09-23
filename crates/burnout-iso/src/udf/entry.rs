//! The file entry of UDF: what a file is, how long it is, and where its
//! bytes are.
//!
//! Each file and each directory has one file entry, in a block of its own.
//! The entry ends with allocation descriptors, which give the runs of blocks
//! that hold the data. A run can hold more descriptors instead of data, and
//! a small file can keep its data in the entry itself.

use std::io::{Read, Seek};

use burnout_core::Result;

use super::tag::{
    name, read_tag, ALLOCATION_EXTENT, EXTENDED_FILE_ENTRY, FILE_ENTRY, INDIRECT_ENTRY,
};
use super::volume::{Address, Volume};
use super::{u16_at, u32_at, u64_at};
use crate::image::{Image, SECTOR};
use crate::node::Extent;

/// The types of file that an entry can give.
pub(crate) const DIRECTORY: u8 = 4;
pub(crate) const FILE: u8 = 5;
pub(crate) const BLOCK_DEVICE: u8 = 6;
pub(crate) const CHARACTER_DEVICE: u8 = 7;
pub(crate) const FIFO: u8 = 9;
pub(crate) const SOCKET: u8 = 10;
pub(crate) const LINK: u8 = 12;

/// The most allocation extent descriptors that one entry may chain.
const MOST_CONTINUATIONS: usize = 4096;

/// A run of the data of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub extent: Extent,
    /// The first block of the run, when the image holds its bytes.
    pub address: Option<Address>,
}

/// What a file entry says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub file_type: u8,
    pub size: u64,
    /// The runs that hold the bytes, in order, cut to the size.
    pub runs: Vec<Run>,
}

/// How the allocation descriptors of an entry give a place.
#[derive(Clone, Copy, Debug)]
enum Form {
    /// Eight bytes, in the partition of the entry.
    Short(u16),
    /// Sixteen bytes, with a partition of their own.
    Long,
}

/// The file entry at `icb`. `what` names it in each error.
pub(crate) fn read_entry<R: Read + Seek>(
    image: &mut Image<R>,
    volume: &Volume,
    icb: Address,
    what: &str,
) -> Result<Entry> {
    let fault = |image: &Image<R>, e: String| image.fault(format!("{what}: {e}"));
    let at = volume.locate(icb, SECTOR).map_err(|e| fault(image, e))?;
    let d = image.read_at(at, SECTOR as usize, what)?;
    let tag = read_tag(&d, icb.block).map_err(|e| fault(image, e))?;
    let (base, lengths) = match tag.id {
        FILE_ENTRY => (176, 168),
        EXTENDED_FILE_ENTRY => (216, 208),
        INDIRECT_ENTRY => return Err(fault(image, strategy(4096))),
        id => {
            return Err(fault(
                image,
                format!(
                    "block {} holds {}, and not a file entry",
                    icb.block,
                    name(id)
                ),
            ))
        }
    };
    if u16_at(&d, 20) != 4 {
        return Err(fault(image, strategy(u16_at(&d, 20))));
    }
    let file_type = d[27];
    let size = u64_at(&d, 56);
    let (attributes, descriptors) = (
        u32_at(&d, lengths) as usize,
        u32_at(&d, lengths + 4) as usize,
    );
    let first = base + attributes;
    let area = first
        .checked_add(descriptors)
        .and_then(|end| d.get(first..end))
        .ok_or_else(|| {
            fault(
                image,
                format!(
                    "its attributes and allocation descriptors take {} bytes, which do not fit its block",
                    attributes as u64 + descriptors as u64
                ),
            )
        })?;
    let runs = match u16_at(&d, 34) & 7 {
        0 => read_runs(image, volume, area, Form::Short(icb.partition), what)?,
        1 => read_runs(image, volume, area, Form::Long, what)?,
        3 => {
            if size > descriptors as u64 {
                return Err(fault(
                    image,
                    format!(
                        "its size is {size}, and it holds {descriptors} bytes of data in its entry"
                    ),
                ));
            }
            let extent = Extent {
                at: at + first as u64,
                length: size,
                zero: false,
            };
            vec![Run {
                extent,
                address: Some(icb),
            }]
        }
        form => {
            return Err(fault(
                image,
                format!("its allocation descriptors are of type {form}, which UDF does not allow"),
            ))
        }
    };
    let runs = cut(runs, size).map_err(|e| fault(image, e))?;
    Ok(Entry {
        file_type,
        size,
        runs,
    })
}

fn strategy(number: u16) -> String {
    format!("its entry is recorded with strategy {number}, and Burnout reads strategy 4")
}

/// The runs that a list of allocation descriptors gives, with each area of
/// more descriptors that the list chains.
fn read_runs<R: Read + Seek>(
    image: &mut Image<R>,
    volume: &Volume,
    first: &[u8],
    form: Form,
    what: &str,
) -> Result<Vec<Run>> {
    let fault = |image: &Image<R>, e: String| image.fault(format!("{what}: {e}"));
    let step = match form {
        Form::Short(_) => 8,
        Form::Long => 16,
    };
    let mut runs = Vec::new();
    let mut area = first.to_vec();
    let mut continuations = 0;
    'areas: loop {
        for ad in area.chunks(step) {
            if ad.len() < step {
                return Err(fault(
                    image,
                    "an allocation descriptor does not fit its area".into(),
                ));
            }
            let raw = u32_at(ad, 0);
            let length = (raw & 0x3FFF_FFFF) as u64;
            // A length of zero ends the list.
            if length == 0 {
                break 'areas;
            }
            let address = match form {
                Form::Short(partition) => Address {
                    block: u32_at(ad, 4),
                    partition,
                },
                Form::Long => Address {
                    block: u32_at(ad, 4),
                    partition: u16_at(ad, 8),
                },
            };
            match raw >> 30 {
                0 => {
                    let at = volume
                        .locate(address, length)
                        .map_err(|e| fault(image, e))?;
                    if at + length > image.length() {
                        return Err(fault(
                            image,
                            "its data runs past the end of the image".into(),
                        ));
                    }
                    let extent = Extent {
                        at,
                        length,
                        zero: false,
                    };
                    runs.push(Run {
                        extent,
                        address: Some(address),
                    });
                }
                // Blocks that were never written read as zero.
                1 | 2 => runs.push(Run {
                    extent: Extent {
                        at: 0,
                        length,
                        zero: true,
                    },
                    address: None,
                }),
                _ => {
                    continuations += 1;
                    if continuations > MOST_CONTINUATIONS {
                        return Err(fault(
                            image,
                            format!("it chains more than {MOST_CONTINUATIONS} areas of allocation descriptors"),
                        ));
                    }
                    area = continuation(image, volume, address, what)?;
                    continue 'areas;
                }
            }
        }
        break;
    }
    Ok(runs)
}

/// The descriptors in the allocation extent descriptor at `address`.
fn continuation<R: Read + Seek>(
    image: &mut Image<R>,
    volume: &Volume,
    address: Address,
    what: &str,
) -> Result<Vec<u8>> {
    let fault = |image: &Image<R>, e: String| image.fault(format!("{what}: {e}"));
    let at = volume
        .locate(address, SECTOR)
        .map_err(|e| fault(image, e))?;
    let d = image.read_at(at, SECTOR as usize, what)?;
    let tag = read_tag(&d, address.block).map_err(|e| fault(image, e))?;
    if tag.id != ALLOCATION_EXTENT {
        return Err(fault(
            image,
            format!(
                "block {} holds {}, and not an allocation extent descriptor",
                address.block,
                name(tag.id)
            ),
        ));
    }
    let length = u32_at(&d, 20) as usize;
    d.get(24..24 + length).map(<[u8]>::to_vec).ok_or_else(|| {
        fault(
            image,
            format!(
                "the allocation extent descriptor at block {} says that it holds {length} bytes",
                address.block
            ),
        )
    })
}

/// The runs, cut to `size` bytes. The runs may hold more than the size, and
/// not less.
fn cut(runs: Vec<Run>, size: u64) -> std::result::Result<Vec<Run>, String> {
    let mut left = size;
    let mut out = Vec::new();
    for mut run in runs {
        if left == 0 {
            break;
        }
        run.extent.length = run.extent.length.min(left);
        left -= run.extent.length;
        out.push(run);
    }
    if left > 0 {
        return Err(format!(
            "its runs hold {} bytes, and its size is {size}",
            size - left
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        allocation_extent, file_entry, long_ad, put, short_ad, tagged, udf_volume, Udf,
        UDF_PARTITION,
    };
    use crate::udf::find_volume;
    use std::io::Cursor;

    /// An image of UDF with a partition of 40 blocks, and a way to put a
    /// descriptor into a block of the partition.
    struct Test {
        image: Vec<u8>,
    }

    impl Test {
        fn new() -> Self {
            Test {
                image: udf_volume(&Udf::default(), 40),
            }
        }

        fn put(&mut self, block: u32, bytes: &[u8]) -> &mut Self {
            put(&mut self.image, UDF_PARTITION + block, bytes);
            self
        }

        fn entry(&self, block: u32) -> Result<Entry> {
            let mut image = Image::new(Cursor::new(self.image.clone()), "test.iso").unwrap();
            let volume = find_volume(&mut image).unwrap().unwrap();
            let icb = Address {
                block,
                partition: 0,
            };
            read_entry(&mut image, &volume, icb, "a.txt")
        }

        fn read(&self, entry: &Entry) -> Vec<u8> {
            let mut bytes = Vec::new();
            for run in &entry.runs {
                let (at, length) = (run.extent.at as usize, run.extent.length as usize);
                match run.extent.zero {
                    true => bytes.resize(bytes.len() + length, 0),
                    false => bytes.extend_from_slice(&self.image[at..at + length]),
                }
            }
            bytes
        }
    }

    fn byte(block: u32) -> u64 {
        (UDF_PARTITION + block) as u64 * 2048
    }

    const RECORDED: u32 = 0;
    const NOT_RECORDED: u32 = 1;
    const NEXT: u32 = 3;

    #[test]
    fn short_descriptors_give_the_runs_in_the_partition_of_the_entry() {
        let ads = [short_ad(RECORDED, 4096, 10), short_ad(RECORDED, 2048, 20)].concat();
        let e = Test::new()
            .put(1, &file_entry(1, false, FILE, 5000, 0, &ads))
            .entry(1)
            .unwrap();
        assert_eq!((e.file_type, e.size), (FILE, 5000));
        let runs: Vec<(u64, u64)> = e
            .runs
            .iter()
            .map(|r| (r.extent.at, r.extent.length))
            .collect();
        assert_eq!(runs, [(byte(10), 4096), (byte(20), 904)]);
        assert_eq!(
            e.runs[1].address,
            Some(Address {
                block: 20,
                partition: 0
            })
        );
    }

    #[test]
    fn long_descriptors_and_an_extended_entry_give_the_same_runs() {
        let ads = [
            long_ad(RECORDED, 4096, 10, 0),
            long_ad(RECORDED, 2048, 20, 0),
        ]
        .concat();
        let e = Test::new()
            .put(1, &file_entry(1, true, FILE, 5000, 1, &ads))
            .entry(1)
            .unwrap();
        let runs: Vec<(u64, u64)> = e
            .runs
            .iter()
            .map(|r| (r.extent.at, r.extent.length))
            .collect();
        assert_eq!(runs, [(byte(10), 4096), (byte(20), 904)]);
    }

    #[test]
    fn data_in_the_entry_is_read_from_the_entry() {
        let mut t = Test::new();
        t.put(1, &file_entry(1, false, FILE, 5, 3, b"hello"));
        let e = t.entry(1).unwrap();
        assert_eq!(t.read(&e), b"hello");
        assert_eq!(
            e.runs[0].address,
            Some(Address {
                block: 1,
                partition: 0
            })
        );
    }

    #[test]
    fn blocks_that_were_never_written_read_as_zero() {
        let ads = [
            short_ad(RECORDED, 2048, 10),
            short_ad(NOT_RECORDED, 4096, 0),
            short_ad(RECORDED, 2048, 11),
        ]
        .concat();
        let mut t = Test::new();
        t.put(1, &file_entry(1, false, FILE, 8192, 0, &ads));
        t.put(10, &[b'a'; 2048]).put(11, &[b'b'; 2048]);
        let bytes = t.read(&t.entry(1).unwrap());
        assert_eq!(bytes.len(), 8192);
        assert!(bytes[..2048].iter().all(|b| *b == b'a'));
        assert!(bytes[2048..6144].iter().all(|b| *b == 0));
        assert!(bytes[6144..].iter().all(|b| *b == b'b'));
    }

    #[test]
    fn an_area_of_more_descriptors_goes_on_with_the_list() {
        let more = [short_ad(RECORDED, 2048, 11), short_ad(RECORDED, 2048, 12)].concat();
        let ads = [short_ad(RECORDED, 2048, 10), short_ad(NEXT, 2048, 30)].concat();
        let mut t = Test::new();
        t.put(1, &file_entry(1, false, FILE, 6144, 0, &ads));
        t.put(30, &allocation_extent(30, &more));
        let e = t.entry(1).unwrap();
        let starts: Vec<u64> = e.runs.iter().map(|r| r.extent.at).collect();
        assert_eq!(starts, [byte(10), byte(11), byte(12)]);
    }

    #[test]
    fn an_area_that_goes_on_to_itself_is_refused() {
        let ads = short_ad(NEXT, 2048, 30);
        let mut t = Test::new();
        t.put(1, &file_entry(1, false, FILE, 2048, 0, &ads));
        t.put(30, &allocation_extent(30, &short_ad(NEXT, 2048, 30)));
        let e = t.entry(1).unwrap_err().to_string();
        assert!(e.contains("a.txt: it chains more than 4096 areas"), "{e}");
    }

    #[test]
    fn runs_that_hold_less_than_the_size_are_refused() {
        let ads = short_ad(RECORDED, 2048, 10);
        let e = Test::new()
            .put(1, &file_entry(1, false, FILE, 3000, 0, &ads))
            .entry(1)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("its runs hold 2048 bytes, and its size is 3000"),
            "{e}"
        );
    }

    #[test]
    fn an_empty_file_has_no_runs() {
        let e = Test::new()
            .put(1, &file_entry(1, false, FILE, 0, 0, &[]))
            .entry(1)
            .unwrap();
        assert!(e.runs.is_empty());
    }

    #[test]
    fn a_run_past_the_end_of_the_partition_is_refused() {
        let ads = short_ad(RECORDED, 4096, 39);
        let e = Test::new()
            .put(1, &file_entry(1, false, FILE, 4096, 0, &ads))
            .entry(1)
            .unwrap_err()
            .to_string();
        assert!(e.contains("run past its end at block 40"), "{e}");
    }

    #[test]
    fn another_strategy_is_refused() {
        let mut fe = file_entry(1, false, FILE, 0, 0, &[]);
        fe[20..22].copy_from_slice(&4096u16.to_le_bytes());
        crate::testing::retag(&mut fe);
        let e = Test::new().put(1, &fe).entry(1).unwrap_err().to_string();
        assert!(e.contains("with strategy 4096"), "{e}");
        let indirect = tagged(259, 1, &[0; 36]);
        let e = Test::new()
            .put(1, &indirect)
            .entry(1)
            .unwrap_err()
            .to_string();
        assert!(e.contains("with strategy 4096"), "{e}");
    }

    #[test]
    fn extended_allocation_descriptors_are_refused() {
        let e = Test::new()
            .put(1, &file_entry(1, false, FILE, 0, 2, &[0; 20]))
            .entry(1)
            .unwrap_err()
            .to_string();
        assert!(e.contains("of type 2, which UDF does not allow"), "{e}");
    }

    #[test]
    fn a_block_that_holds_another_descriptor_is_refused() {
        let e = Test::new()
            .put(1, &tagged(256, 1, &[0; 496]))
            .entry(1)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("holds a file set descriptor, and not a file entry"),
            "{e}"
        );
    }

    #[test]
    fn descriptors_that_do_not_fit_their_block_are_refused() {
        let mut fe = file_entry(1, false, FILE, 0, 0, &[]);
        fe[172..176].copy_from_slice(&5000u32.to_le_bytes());
        crate::testing::retag(&mut fe);
        let e = Test::new().put(1, &fe).entry(1).unwrap_err().to_string();
        assert!(e.contains("which do not fit its block"), "{e}");
    }
}
