//! The bytes of one resource of a WIM file, decompressed.
//!
//! A compressed resource starts with a table of its chunks: one entry for
//! each chunk but the first, which gives where the chunk starts after the
//! table. An entry has 4 bytes, or 8 when the resource holds more than
//! 4 GiB. A chunk that compression did not make smaller is stored as it is.

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};

use burnout_core::{Error, Result};

use crate::blobs::Blob;
use crate::header::{Compression, Header, Resource};
use crate::{lzms, lzx, xpress};

/// The decompressor of the chunks, and what it needs.
#[derive(Clone, Copy)]
enum Decoder {
    /// LZX, with the order of its window.
    Lzx(u32),
    Xpress,
    Lzms,
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

    // The tables of LZMS take some 300 KiB, so the chunks of one resource
    // share them.
    let mut lzms = matches!(decoder, Decoder::Lzms).then(lzms::Decompressor::new);
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
                Decoder::Lzms => lzms
                    .as_mut()
                    .expect("made for LZMS")
                    .decompress(&input, target)
                    .map_err(|e| e.to_string()),
            }
            .map_err(|e| fault(format!("chunk {i}: {e}")))?;
        }
        out.write_all(target)?;
    }
    Ok(())
}

/// Reads the streams of a WIM file, those with a resource of their own and
/// those in a solid resource.
///
/// A solid resource is decompressed one chunk at a time, and the reader
/// keeps the last chunk. So read the streams of a solid resource in the
/// order of their offsets: then each chunk is decompressed once.
#[derive(Default)]
pub struct BlobReader {
    /// The chunks of each solid resource, by the offset of the resource.
    indexes: HashMap<u64, SolidIndex>,
    /// The last chunk: the offset of its resource, its number, and its bytes.
    chunk: Option<(u64, u64, Vec<u8>)>,
    lzms: Option<lzms::Decompressor>,
}

/// Where the chunks of one solid resource are, from its own header.
struct SolidIndex {
    /// The bytes of the resource after decompression.
    bytes: u64,
    chunk_bytes: u64,
    decoder: Option<Decoder>,
    /// Where each chunk starts in the file, and where the last one ends.
    starts: Vec<u64>,
}

/// The bytes before the table of chunks of a solid resource: its size after
/// decompression, the size of a chunk, and the number of its compression.
const SOLID_HEADER_BYTES: u64 = 16;

impl BlobReader {
    pub fn new() -> BlobReader {
        BlobReader::default()
    }

    /// Read the bytes of `blob`, from the WIM file that `reader` gives.
    /// `name` names the file in an error.
    pub fn read<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        header: &Header,
        blob: &Blob,
        name: &str,
    ) -> Result<Vec<u8>> {
        if blob.solid.is_none() {
            return read_resource(reader, header, &blob.resource, name);
        }
        if blob.bytes() > MEMORY_LIMIT {
            return Err(Error::Image {
                path: name.to_string(),
                detail: format!(
                    "a stream holds {} bytes, more than Burnout reads at once",
                    blob.bytes()
                ),
            });
        }
        let mut out = Vec::with_capacity(blob.bytes() as usize);
        self.copy(reader, header, blob, name, &mut out)?;
        Ok(out)
    }

    /// Write the bytes of `blob`, from the WIM file that `reader` gives, to
    /// `out`. `name` names the file in an error.
    pub fn copy<R: Read + Seek, W: Write>(
        &mut self,
        reader: &mut R,
        header: &Header,
        blob: &Blob,
        name: &str,
        out: &mut W,
    ) -> Result<()> {
        let Some(run) = &blob.solid else {
            return copy_resource(reader, header, &blob.resource, name, out);
        };
        let fault = |detail: String| Error::Image {
            path: name.to_string(),
            detail,
        };
        let mut at = blob.resource.offset;
        let mut left = blob.bytes();
        // The resources of a run hold their bytes one after the other.
        let mut base = 0u64;
        for resource in run.iter() {
            if left == 0 {
                break;
            }
            let size = self.index(reader, resource, name)?.bytes;
            let end = base + size;
            if at < end {
                let chunk_bytes = self.indexes[&resource.offset].chunk_bytes;
                while left > 0 && at < end {
                    let number = (at - base) / chunk_bytes;
                    let bytes = self.chunk(reader, resource, number, name)?;
                    let from = (at - base - number * chunk_bytes) as usize;
                    let take = (bytes.len() - from).min(left.min(usize::MAX as u64) as usize);
                    out.write_all(&bytes[from..from + take])?;
                    at += take as u64;
                    left -= take as u64;
                }
            }
            base = end;
        }
        if left > 0 {
            return Err(fault(format!(
                "a stream ends {left} bytes after the end of its solid resources"
            )));
        }
        Ok(())
    }

    /// The chunks of `resource`, read from its header the first time.
    fn index<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        resource: &Resource,
        name: &str,
    ) -> Result<&SolidIndex> {
        if let std::collections::hash_map::Entry::Vacant(e) = self.indexes.entry(resource.offset) {
            e.insert(solid_index(reader, resource, name)?);
        }
        Ok(&self.indexes[&resource.offset])
    }

    /// The bytes of chunk `number` of `resource`, decompressed.
    fn chunk<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        resource: &Resource,
        number: u64,
        name: &str,
    ) -> Result<&[u8]> {
        let key = (resource.offset, number);
        if !matches!(&self.chunk, Some((r, n, _)) if (*r, *n) == key) {
            let fault = |detail: String| Error::Image {
                path: name.to_string(),
                detail: format!("the solid resource at byte {}: {detail}", resource.offset),
            };
            let index = &self.indexes[&resource.offset];
            let i = number as usize;
            let (from, to) = (index.starts[i], index.starts[i + 1]);
            let size = (index.bytes - number * index.chunk_bytes).min(index.chunk_bytes) as usize;
            let mut input = vec![0u8; (to - from) as usize];
            reader.seek(SeekFrom::Start(from))?;
            reader
                .read_exact(&mut input)
                .map_err(|_| fault("the file ends before the resource does".to_string()))?;
            let mut output = match self.chunk.take() {
                Some((_, _, mut v)) => {
                    v.clear();
                    v
                }
                None => Vec::new(),
            };
            output.resize(size, 0);
            if input.len() == size {
                output.copy_from_slice(&input);
            } else if input.len() > size {
                return Err(fault(format!("chunk {i} is stored larger than it is")));
            } else {
                match index.decoder {
                    Some(Decoder::Lzx(order)) => lzx::decompress(&input, &mut output, order),
                    Some(Decoder::Xpress) => xpress::decompress(&input, &mut output),
                    Some(Decoder::Lzms) => self
                        .lzms
                        .get_or_insert_with(lzms::Decompressor::new)
                        .decompress(&input, &mut output)
                        .map_err(|e| e.to_string()),
                    None => {
                        Err("it is compressed, and its header names no compression".to_string())
                    }
                }
                .map_err(|e| fault(format!("chunk {i}: {e}")))?;
            }
            self.chunk = Some((key.0, key.1, output));
        }
        Ok(&self.chunk.as_ref().expect("the chunk is there").2)
    }
}

/// Read the header and the table of chunks of a solid resource.
fn solid_index<R: Read + Seek>(
    reader: &mut R,
    resource: &Resource,
    name: &str,
) -> Result<SolidIndex> {
    let fault = |detail: String| Error::Image {
        path: name.to_string(),
        detail: format!("the solid resource at byte {}: {detail}", resource.offset),
    };
    let short = || fault("the file ends before the resource does".to_string());
    let mut head = [0u8; SOLID_HEADER_BYTES as usize];
    reader.seek(SeekFrom::Start(resource.offset))?;
    reader.read_exact(&mut head).map_err(|_| short())?;
    let bytes = u64::from_le_bytes(head[..8].try_into().unwrap());
    let chunk_bytes = u32::from_le_bytes(head[8..12].try_into().unwrap()) as u64;
    let format = u32::from_le_bytes(head[12..16].try_into().unwrap());
    if chunk_bytes == 0 {
        return Err(fault("its header gives chunks of 0 bytes".to_string()));
    }
    let decoder = match format {
        0 => None,
        1 => Some(Decoder::Xpress),
        2 => Some(Decoder::Lzx(
            lzx::window_order(chunk_bytes as u32)
                .ok_or_else(|| fault(format!("LZX with chunks of {chunk_bytes} bytes")))?,
        )),
        3 => Some(Decoder::Lzms),
        other => return Err(fault(format!("its header names compression {other}"))),
    };
    let count = bytes.div_ceil(chunk_bytes);
    let table = count * 4;
    if SOLID_HEADER_BYTES + table > resource.stored_bytes || chunk_bytes > MEMORY_LIMIT {
        return Err(fault(
            "its table of chunks is larger than the resource".to_string(),
        ));
    }
    let mut sizes = vec![0u8; table as usize];
    reader.read_exact(&mut sizes).map_err(|_| short())?;
    // The table gives the size of each chunk, and not where it starts.
    let mut starts = Vec::with_capacity(count as usize + 1);
    let mut at = resource.offset + SOLID_HEADER_BYTES + table;
    starts.push(at);
    for size in sizes.chunks_exact(4) {
        at += u32::from_le_bytes(size.try_into().unwrap()) as u64;
        starts.push(at);
    }
    if at > resource.offset + resource.stored_bytes {
        return Err(fault("its chunks end after the resource does".to_string()));
    }
    Ok(SolidIndex {
        bytes,
        chunk_bytes,
        decoder,
        starts,
    })
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
        Compression::Lzms => Ok(Decoder::Lzms),
        Compression::None => {
            Err("it is compressed, and the WIM header names no compression".to_string())
        }
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
        let mut b = header(0x0004_0002, 1, (0, 0));
        b[0x14..0x18].copy_from_slice(&0u32.to_le_bytes());
        let no_chunks = Header::parse(&b, "a.wim").unwrap();
        let r = resource(0, 4, 8, Resource::COMPRESSED);
        let e = read_resource(&mut Cursor::new(vec![0u8; 8]), &no_chunks, &r, "a.wim").unwrap_err();
        assert!(e.to_string().contains("chunks of 0 bytes"), "{e}");
    }

    /// A solid resource at byte 0 of a file: its header, its table of chunk
    /// sizes and its chunks, stored as they are.
    fn solid_file(bytes: u64, chunk: u32, format: u32, sizes: &[u32], data: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&bytes.to_le_bytes());
        f.extend_from_slice(&chunk.to_le_bytes());
        f.extend_from_slice(&format.to_le_bytes());
        for s in sizes {
            f.extend_from_slice(&s.to_le_bytes());
        }
        f.extend_from_slice(data);
        f
    }

    fn solid_blob(file_bytes: u64, offset: u64, size: u64) -> Blob {
        let run: std::sync::Arc<[Resource]> = std::sync::Arc::from(vec![resource(
            0,
            file_bytes,
            crate::blobs::SOLID_RESOURCE,
            Resource::SOLID | Resource::COMPRESSED,
        )]);
        Blob {
            resource: resource(offset, size, 0, Resource::SOLID),
            part: 1,
            references: 1,
            hash: [0; 20],
            solid: Some(run),
        }
    }

    #[test]
    fn a_stream_reads_across_the_chunks_of_a_solid_resource() {
        // Chunks of 4 bytes, 10 bytes in all, each stored as it is.
        let file = solid_file(10, 4, 3, &[4, 4, 2], b"abcdefghij");
        let h = lzx_header(32768);
        let mut reader = BlobReader::new();
        let mut cursor = Cursor::new(file.clone());
        let blob = solid_blob(file.len() as u64, 2, 7);
        assert_eq!(
            reader.read(&mut cursor, &h, &blob, "a.esd").unwrap(),
            b"cdefghi"
        );
        let tail = solid_blob(file.len() as u64, 9, 1);
        assert_eq!(reader.read(&mut cursor, &h, &tail, "a.esd").unwrap(), b"j");
        let past = solid_blob(file.len() as u64, 8, 5);
        let e = reader.read(&mut cursor, &h, &past, "a.esd").unwrap_err();
        assert!(e.to_string().contains("after the end of its solid"), "{e}");
    }

    #[test]
    fn a_damaged_solid_resource_is_refused() {
        let h = lzx_header(32768);
        let cases = [
            (solid_file(10, 0, 3, &[], b""), "chunks of 0 bytes"),
            (
                solid_file(10, 4, 9, &[4, 4, 2], b"abcdefghij"),
                "compression 9",
            ),
            (
                solid_file(10, 4, 3, &[4, 4, 9], b"abcdefghij"),
                "end after the resource",
            ),
            (
                solid_file(1 << 20, 4, 3, &[], b""),
                "larger than the resource",
            ),
        ];
        for (file, says) in cases {
            let blob = solid_blob(file.len() as u64, 0, 1);
            let e = BlobReader::new()
                .read(&mut Cursor::new(file), &h, &blob, "a.esd")
                .unwrap_err();
            assert!(e.to_string().contains(says), "{says}: {e}");
        }
    }
}
