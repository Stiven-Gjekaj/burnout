//! Read one file of a tree, and take its digest on the way.
//!
//! Each copy reads a file the same way: in whole chunks, with a check that
//! the file holds the bytes that the tree says it holds. Only where the bytes
//! go is different, so that is the one part that a copy gives.

use std::io::{self, Read};

use burnout_core::{Digest, Error, Result, Sha256};

use burnout_core::TreePath;

/// How much of a file one read takes.
pub(crate) const CHUNK_BYTES: usize = 1024 * 1024;

/// Read the file at `path` from `reader`, and give each piece of it to
/// `sink`.
///
/// The tree says that the file holds `bytes` bytes. Each piece fills
/// `chunk`, except the last one, so a sink that writes whole sectors can
/// write each piece but the last as it comes. A file that gives more or fewer
/// bytes than the tree said is refused, and no byte past the size reaches
/// `sink`.
///
/// The digest is of the bytes that went to `sink`.
pub(crate) fn stream_file<R: Read + ?Sized>(
    reader: &mut R,
    path: &TreePath,
    bytes: u64,
    chunk: &mut [u8],
    mut sink: impl FnMut(&[u8]) -> io::Result<()>,
) -> Result<Digest> {
    let mut hash = Sha256::new();
    let mut done: u64 = 0;
    loop {
        let n = fill(reader, chunk).map_err(|e| Error::Source {
            path: path.to_string(),
            detail: e.to_string(),
        })?;
        if n == 0 {
            break;
        }
        if done + n as u64 > bytes {
            return Err(wrong_size(path, bytes, &format!("more than {bytes}")));
        }
        done += n as u64;
        hash.update(&chunk[..n]);
        sink(&chunk[..n]).map_err(|e| Error::CannotCopy {
            path: path.to_string(),
            detail: e.to_string(),
        })?;
        if n < chunk.len() {
            break;
        }
    }
    if done != bytes {
        return Err(wrong_size(path, bytes, &done.to_string()));
    }
    Ok(hash.finish())
}

/// Read until `buf` is full or the reader ends, and give how much it holds.
fn fill<R: Read + ?Sized>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn wrong_size(path: &TreePath, bytes: u64, gave: &str) -> Error {
    Error::Source {
        path: path.to_string(),
        detail: format!("the tree gave its size as {bytes} bytes, and the file gave {gave}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burnout_core::sha256;

    /// A reader that gives one byte for each read, as a slow pipe can.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match (self.0.split_first(), buf.first_mut()) {
                (Some((byte, rest)), Some(slot)) => {
                    *slot = *byte;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    fn path() -> TreePath {
        TreePath::new("sources/install.wim").unwrap()
    }

    #[test]
    fn each_piece_but_the_last_fills_the_chunk() {
        let bytes: Vec<u8> = (0..2500u32).map(|n| n as u8).collect();
        let mut chunk = [0u8; 1000];
        let mut pieces = Vec::new();
        let mut out = Vec::new();
        let digest = stream_file(&mut Trickle(&bytes), &path(), 2500, &mut chunk, |p| {
            pieces.push(p.len());
            out.extend_from_slice(p);
            Ok(())
        })
        .unwrap();
        assert_eq!(pieces, [1000, 1000, 500]);
        assert_eq!(out, bytes);
        assert_eq!(digest, sha256(&bytes));
    }

    #[test]
    fn a_file_of_whole_chunks_ends_on_a_whole_chunk() {
        let bytes = vec![7u8; 2000];
        let mut chunk = [0u8; 1000];
        let mut pieces = Vec::new();
        stream_file(&mut bytes.as_slice(), &path(), 2000, &mut chunk, |p| {
            pieces.push(p.len());
            Ok(())
        })
        .unwrap();
        assert_eq!(pieces, [1000, 1000]);
    }

    #[test]
    fn a_file_that_gives_more_than_it_said_sends_nothing_past_the_size() {
        let bytes = vec![1u8; 30];
        let mut chunk = [0u8; 8];
        let mut sent = 0;
        let e = stream_file(&mut bytes.as_slice(), &path(), 20, &mut chunk, |p| {
            sent += p.len();
            Ok(())
        })
        .unwrap_err();
        assert!(sent <= 20, "{sent} bytes reached the sink");
        assert_eq!(
            e.to_string(),
            "cannot use sources/install.wim: the tree gave its size as 20 bytes, and the file gave more than 20"
        );
    }

    #[test]
    fn a_file_that_gives_fewer_than_it_said_is_refused() {
        let mut chunk = [0u8; 8];
        let e = stream_file(
            &mut [1u8; 5].as_slice(),
            &path(),
            10,
            &mut chunk,
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(e, Error::Source { .. }), "{e}");
        assert!(e.to_string().ends_with("and the file gave 5"), "{e}");
    }

    #[test]
    fn a_fault_of_the_sink_is_a_fault_of_the_copy() {
        let mut chunk = [0u8; 8];
        let e = stream_file(&mut [1u8; 5].as_slice(), &path(), 5, &mut chunk, |_| {
            Err(io::Error::other("the volume is full"))
        })
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "cannot copy sources/install.wim onto the volume: the volume is full"
        );
    }
}
