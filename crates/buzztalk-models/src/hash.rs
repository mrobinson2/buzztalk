//! SHA-256 hashing helpers.
//!
//! Downloads are hashed in the same pass they're written to disk (see
//! [`HashingWriter`]) rather than read back afterwards -- for a 166 MB
//! bundle that's the difference between one pass over the bytes and two.

use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Render a digest as lowercase hex, matching the format the pinned
/// checksums in [`crate::manifest`] and `shasum -a 256` both use.
fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A [`Write`] wrapper that feeds every byte through a SHA-256 hasher as
/// well as the inner writer, so a download is hashed in the same streaming
/// pass that writes it to the staging file.
pub struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> HashingWriter<W> {
    /// Wrap `inner` so every write is also hashed.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// Consume the wrapper, returning the inner writer and the lowercase
    /// hex SHA-256 digest of everything written through it.
    pub fn finish(self) -> (W, String) {
        let digest = self.hasher.finalize();
        (self.inner, to_hex(&digest))
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Hash an existing file on disk, streaming it in chunks rather than
/// loading it whole into memory.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_writer_matches_known_digest() {
        // sha256("hello world") -- a well-known test vector.
        let mut w = HashingWriter::new(Vec::new());
        w.write_all(b"hello world").unwrap();
        let (buf, digest) = w.finish();
        assert_eq!(buf, b"hello world");
        assert_eq!(
            digest,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn hash_file_matches_hashing_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"some content to hash").unwrap();

        let mut w = HashingWriter::new(Vec::new());
        w.write_all(b"some content to hash").unwrap();
        let (_, expected) = w.finish();

        assert_eq!(hash_file(&path).unwrap(), expected);
    }
}
