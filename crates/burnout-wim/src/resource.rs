//! The bytes of one resource of a WIM file, decompressed.
//!
//! A compressed resource starts with a table of its chunks: one entry for
//! each chunk but the first, which gives where the chunk starts after the
//! table. An entry has 4 bytes, or 8 when the resource holds more than
//! 4 GiB. A chunk that compression did not make smaller is stored as it is.

use std::io::{self, Read, Seek, SeekFrom, Write};

use burnout_core::{Error, Result};

use crate::header::{Compression, Header, Resource};
use crate::{lzx, xpress};

/// The decompressor of the chunks, and what it needs.
#[derive(Clone, Copy)]
enum Decoder {
    /// LZX, with the order of its window.
    Lzx(u32),
    Xpress,
}

/// The largest resource that `read_resource` holds in memory at once.
/// `copy_resource` takes any size.
const MEMORY_LIMIT: u64 = 1 << 32;

/// Read and decompress `resource` of the WIM file that `reader` gives.
/// `name` names the file in an error.
pub fn read_resource<R: Read + Seek>(
    reader: &mut R,
    header: &Header,
    resource: &Resource,
    name: &str,
) -> Result<Vec<u8>> {
    if resource.original_bytes > MEMORY_LIMIT {
        return Err(Error::Image {
            path: name.to_string(),
            detail: format!(
                "the resource at byte {} holds {} bytes, more than Burnout reads at once",
                resource.offset, resource.original_bytes
            ),
        });
    }
    let mut out = Vec::with_capacity(resource.original_bytes as usize);
    copy_resource(reader, header, resource, name, &mut out)?;
    Ok(out)
}

/// Decompress `resource` of the WIM file that `reader` gives into `out`, one
/// chunk at a time, so a resource of any size takes the memory of one chunk.
/// `name` names the file in an error.
pub fn copy_resource<R: Read + Seek, W: Write>(
    reader: &mut R,
    header: &Header,
    resource: &Resource,
    name: &str,
    out: &mut W,
) -> Result<()> {
    let at = resource.offset;
    let fault = |detail: String| Error::Image {
        path: name.to_string(),
        detail: format!("the resource at byte {at}: {detail}"),
    };
    let short = || fault("the file ends before the resource does".to_string());
    if resource.flags & Resource::SOLID != 0 {
        return Err(fault(
            "it is solid, and Burnout does not read a solid resource yet".to_string(),
        ));
    }
    reader.seek(SeekFrom::Start(at))?;
    if !resource.is_compressed() {
        if resource.stored_bytes != resource.original_bytes {
            return Err(fault(
                "it is not compressed, and its two sizes differ".to_string(),
            ));
        }
        let copied = io::copy(&mut reader.take(resource.stored_bytes), out)?;
        if copied != resource.stored_bytes {
            return Err(short());
        }
        return Ok(());
    }

    let decoder = decoder(header).map_err(fault)?;
    let chunk = header.chunk_bytes as u64;
    let original = resource.original_bytes;
    let count = original.div_ceil(chunk);
    let entry: u64 = if original > u32::MAX as u64 { 8 } else { 4 };
    let table = count.saturating_sub(1) * entry;
    if table > resource.stored_bytes {
        return Err(fault(
            "its table of chunks is larger than the resource".to_string(),
        ));
    }
    let data_bytes = resource.stored_bytes - table;
    let mut table_bytes = vec![0u8; table as usize];
    reader.read_exact(&mut table_bytes).map_err(|_| short())?;
    let mut starts = Vec::with_capacity(count as usize + 1);
    starts.push(0u64);
    for e in table_bytes.chunks_exact(entry as usize) {
        starts.push(match entry {
            4 => u32::from_le_bytes(e.try_into().unwrap()) as u64,
            _ => u64::from_le_bytes(e.try_into().unwrap()),
        });
    }
    starts.push(data_bytes);

    let mut input = Vec::new();
    let mut output = vec![0u8; chunk as usize];
    for i in 0..count as usize {
        let (from, to) = (starts[i], starts[i + 1]);
        if from > to || to > data_bytes {
            return Err(fault(format!(
                "chunk {i} has a place that its table cannot give"
            )));
        }
        input.resize((to - from) as usize, 0);
        reader.read_exact(&mut input).map_err(|_| short())?;
        let size = (original - i as u64 * chunk).min(chunk) as usize;
        let target = &mut output[..size];
        if input.len() == size {
            target.copy_from_slice(&input);
        } else if input.len() > size {
            return Err(fault(format!("chunk {i} is stored larger than it is")));
        } else {
            match decoder {
                Decoder::Lzx(order) => lzx::decompress(&input, target, order),
                Decoder::Xpress => xpress::decompress(&input, target),
            }
            .map_err(|e| fault(format!("chunk {i}: {e}")))?;
        }
        out.write_all(target)?;
    }
    Ok(())
}

/// The decompressor that the header names.
fn decoder(header: &Header) -> std::result::Result<Decoder, String> {
    if header.chunk_bytes == 0 {
        return Err("the WIM header gives chunks of 0 bytes".to_string());
    }
    match header.compression() {
        Compression::Lzx => lzx::window_order(header.chunk_bytes)
            .map(Decoder::Lzx)
            .ok_or_else(|| format!("LZX with chunks of {} bytes", header.chunk_bytes)),
        Compression::Xpress => Ok(Decoder::Xpress),
        Compression::None => {
            Err("it is compressed, and the WIM header names no compression".to_string())
        }
        other => Err(format!(
            "Burnout does not read {} compression yet",
            other.name()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::tests::header;
    use std::io::Cursor;

    fn lzx_header(chunk: u32) -> Header {
        let mut b = header(0x0004_0002, 1, (0, 0));
        b[0x14..0x18].copy_from_slice(&chunk.to_le_bytes());
        Header::parse(&b, "a.wim").unwrap()
    }

    fn resource(offset: u64, stored: u64, original: u64, flags: u8) -> Resource {
        Resource {
            stored_bytes: stored,
            flags,
            offset,
            original_bytes: original,
        }
    }

    #[test]
    fn a_resource_that_is_not_compressed_reads_as_it_is() {
        let file = b"xxxxhello".to_vec();
        let h = lzx_header(32768);
        let r = resource(4, 5, 5, 0);
        let got = read_resource(&mut Cursor::new(file), &h, &r, "a.wim").unwrap();
        assert_eq!(got, b"hello");
    }

    #[test]
    fn chunks_that_compression_did_not_shrink_are_copied() {
        // Chunks of 4 bytes, 10 bytes in all: two entries in the table.
        let mut file = Vec::new();
        file.extend_from_slice(&4u32.to_le_bytes());
        file.extend_from_slice(&8u32.to_le_bytes());
        file.extend_from_slice(b"abcdefghij");
        let mut b = header(0x0004_0002, 1, (0, 0));
        b[0x14..0x18].copy_from_slice(&4u32.to_le_bytes());
        let h = Header::parse(&b, "a.wim").unwrap();
        let r = resource(0, file.len() as u64, 10, Resource::COMPRESSED);
        let got = read_resource(&mut Cursor::new(file), &h, &r, "a.wim").unwrap();
        assert_eq!(got, b"abcdefghij");
    }

    #[test]
    fn a_damaged_or_unknown_resource_is_refused() {
        let h = lzx_header(32768);
        let cases = [
            (resource(0, 4, 4, Resource::SOLID), "is solid"),
            (resource(0, 4, 5, 0), "two sizes differ"),
            (resource(0, 100, 100, 0), "ends before the resource does"),
            (
                resource(0, 4, 70000, Resource::COMPRESSED),
                "larger than the resource",
            ),
        ];
        for (r, says) in cases {
            let e = read_resource(&mut Cursor::new(vec![0u8; 8]), &h, &r, "a.wim").unwrap_err();
            assert!(e.to_string().contains(says), "{says}: {e}");
        }
        let mut b = header(0x0008_0002, 1, (0, 0));
        b[0x14..0x18].copy_from_slice(&32768u32.to_le_bytes());
        let lzms = Header::parse(&b, "a.wim").unwrap();
        let r = resource(0, 4, 8, Resource::COMPRESSED);
        let e = read_resource(&mut Cursor::new(vec![0u8; 8]), &lzms, &r, "a.wim").unwrap_err();
        assert!(e.to_string().contains("LZMS compression yet"), "{e}");
    }
}
