//! The bytes of one resource of a WIM file, decompressed.
//!
//! A compressed resource starts with a table of its chunks: one entry for
//! each chunk but the first, which gives where the chunk starts after the
//! table. An entry has 4 bytes, or 8 when the resource holds more than
//! 4 GiB. A chunk that compression did not make smaller is stored as it is.

use std::io::{Read, Seek, SeekFrom};

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

/// The largest resource that this reader holds in memory at once.
const MEMORY_LIMIT: u64 = 1 << 32;

/// Read and decompress `resource` of the WIM file that `reader` gives.
/// `name` names the file in an error.
pub fn read_resource<R: Read + Seek>(
    reader: &mut R,
    header: &Header,
    resource: &Resource,
    name: &str,
) -> Result<Vec<u8>> {
    let fault = |detail: String| Error::Image {
        path: name.to_string(),
        detail,
    };
    let at = resource.offset;
    if resource.flags & Resource::SOLID != 0 {
        return Err(fault(format!(
            "the resource at byte {at} is solid, and Burnout does not read a solid resource yet"
        )));
    }
    if resource.original_bytes > MEMORY_LIMIT {
        return Err(fault(format!(
            "the resource at byte {at} holds {} bytes, more than Burnout reads at once",
            resource.original_bytes
        )));
    }
    let mut stored = vec![0u8; resource.stored_bytes as usize];
    reader.seek(SeekFrom::Start(at))?;
    reader.read_exact(&mut stored).map_err(|_| {
        fault(format!(
            "it ends before the end of the resource at byte {at}"
        ))
    })?;
    if !resource.is_compressed() {
        if resource.stored_bytes != resource.original_bytes {
            return Err(fault(format!(
                "the resource at byte {at} is not compressed, and its two sizes differ"
            )));
        }
        return Ok(stored);
    }
    decompress_chunks(&stored, header, resource.original_bytes)
        .map_err(|e| fault(format!("the resource at byte {at}: {e}")))
}

/// The bytes of a compressed resource, from its stored bytes.
fn decompress_chunks(
    stored: &[u8],
    header: &Header,
    original: u64,
) -> std::result::Result<Vec<u8>, String> {
    let chunk = header.chunk_bytes as u64;
    if chunk == 0 {
        return Err("the WIM header gives chunks of 0 bytes".to_string());
    }
    let compression = header.compression();
    let decoder = match compression {
        Compression::Lzx => Decoder::Lzx(
            lzx::window_order(header.chunk_bytes)
                .ok_or_else(|| format!("LZX with chunks of {} bytes", header.chunk_bytes))?,
        ),
        Compression::Xpress => Decoder::Xpress,
        Compression::None => {
            return Err(
                "the resource is compressed, and the WIM header names no compression".to_string(),
            )
        }
        other => {
            return Err(format!(
                "Burnout does not read {} compression yet",
                other.name()
            ))
        }
    };
    let count = original.div_ceil(chunk) as usize;
    let entry = if original > u32::MAX as u64 { 8 } else { 4 };
    let table = (count.saturating_sub(1)) * entry;
    if table > stored.len() {
        return Err("its table of chunks is larger than the resource".to_string());
    }
    let data = &stored[table..];
    let mut starts = Vec::with_capacity(count + 1);
    starts.push(0u64);
    for e in stored[..table].chunks_exact(entry) {
        starts.push(match entry {
            4 => u32::from_le_bytes(e.try_into().unwrap()) as u64,
            _ => u64::from_le_bytes(e.try_into().unwrap()),
        });
    }
    starts.push(data.len() as u64);

    let mut out = vec![0u8; original as usize];
    for i in 0..count {
        let (from, to) = (starts[i], starts[i + 1]);
        if from > to || to > data.len() as u64 {
            return Err(format!("chunk {i} has a place that its table cannot give"));
        }
        let input = &data[from as usize..to as usize];
        let first = i * chunk as usize;
        let size = (original - first as u64).min(chunk) as usize;
        let target = &mut out[first..first + size];
        if input.len() == size {
            target.copy_from_slice(input);
        } else if input.len() > size {
            return Err(format!("chunk {i} is stored larger than it is"));
        } else {
            match decoder {
                Decoder::Lzx(order) => lzx::decompress(input, target, order),
                Decoder::Xpress => xpress::decompress(input, target),
            }
            .map_err(|e| format!("chunk {i}: {e}"))?;
        }
    }
    Ok(out)
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
            (
                resource(0, 100, 100, 0),
                "ends before the end of the resource",
            ),
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
