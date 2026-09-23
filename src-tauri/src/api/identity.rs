use std::io::Read;
use std::path::Path;

/// Bytes Fireshare hashes to identify a video. Must match `util.video_id`'s
/// `mb=16` exactly, or the answer means nothing.
const HEADER_BYTES: usize = 16 * 1024 * 1024;

/// The id Fireshare files a video under: an xxh3_128 digest of the first 16 MB,
/// as 32 lowercase hex characters.
///
/// Computed locally so a folder can be checked against the library before
/// anything is transferred. The duplicate rejection on the upload routes only
/// fires once the file is on the server's disk, which for a chunked upload means
/// the whole thing crossed the network before the 409 came back — the exact
/// situation a backlog of already-uploaded clips produces.
///
/// Reads only the header, so the cost is the same for a 4 GB file as a 20 MB one.
pub fn video_id(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut header = Vec::with_capacity(HEADER_BYTES.min(1 << 20));
    file.by_ref().take(HEADER_BYTES as u64).read_to_end(&mut header)?;
    Ok(format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&header)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Pinned against values produced by Python's xxhash, the library Fireshare
    /// itself uses. A digest that merely looks like a hash is worthless here:
    /// if it disagrees with the server's, every existence check silently says
    /// "not in the library" and the feature quietly does nothing.
    #[test]
    fn matches_the_digests_python_produces() {
        let dir = std::env::temp_dir().join(format!("firesync-id-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        for (bytes, expected) in [
            (b"".to_vec(), "99aa06d3014798d86001c324468d497f"),
            (b"hello".to_vec(), "b5e9c1ad071b3e7fc779cfaa5e523818"),
            (vec![0u8; 1024], "0717191e67688313de5f15ab6daf7941"),
        ] {
            let path = dir.join("probe.bin");
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(video_id(&path).unwrap(), expected, "for {} bytes", bytes.len());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only the first 16 MB counts, so two files that differ past it share an
    /// id — which is Fireshare's behaviour, not a bug to paper over here.
    #[test]
    fn only_the_header_is_read() {
        let dir = std::env::temp_dir().join(format!("firesync-id-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        for (path, tail) in [(&a, 1u8), (&b, 2u8)] {
            let mut f = std::fs::File::create(path).unwrap();
            f.write_all(&vec![7u8; HEADER_BYTES]).unwrap();
            f.write_all(&[tail; 4096]).unwrap();
        }

        assert_eq!(video_id(&a).unwrap(), video_id(&b).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
