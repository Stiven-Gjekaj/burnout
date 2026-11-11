//! Each stream of a WIM file that wimlib made decompresses to the SHA-1 that
//! the offset table gives it. `tests/data/make.sh` makes the files.

use std::fs::File;
use std::path::Path;

use burnout_wim::sha1::sha1;
use burnout_wim::{read_blobs, read_resource, read_wim, Compression};

fn check(file: &str, compression: Compression, chunk: u32) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(file);
    let wim = read_wim(File::open(&path).unwrap(), file).unwrap();
    assert_eq!(wim.header.compression(), compression);
    assert_eq!(wim.header.chunk_bytes, chunk);
    assert_eq!(wim.editions.len(), 1);
    assert_eq!(wim.editions[0].name, "Test");

    let mut reader = File::open(&path).unwrap();
    let blobs = read_blobs(&mut reader, &wim.header, file).unwrap();
    // Four files hold bytes, and one stream is the metadata.
    assert_eq!(blobs.len(), 5, "{blobs:#?}");
    assert_eq!(blobs.iter().filter(|b| b.is_metadata()).count(), 1);
    let mut compressed = 0;
    for blob in &blobs {
        let bytes = read_resource(&mut reader, &wim.header, &blob.resource, file).unwrap();
        assert_eq!(bytes.len() as u64, blob.resource.original_bytes);
        assert_eq!(
            sha1(&bytes),
            blob.hash,
            "the stream at byte {}",
            blob.resource.offset
        );
        compressed += blob.resource.is_compressed() as usize;
    }
    assert!(compressed >= 3, "only {compressed} streams were compressed");
}

#[test]
fn lzx_with_chunks_of_32_kib() {
    check("lzx-32k.wim", Compression::Lzx, 32768);
}

#[test]
fn lzx_with_chunks_of_64_kib() {
    check("lzx-64k.wim", Compression::Lzx, 65536);
}
