//! Abstraction over "fetch the bytes of a URL", so install logic is
//! testable without touching the network.
//!
//! [`UreqFetcher`] is the real implementation used by [`crate::ensure_models`].
//! Tests substitute an in-memory fake (see `tests` below and
//! `tests/install_tests.rs`) -- no test in this crate that exercises
//! [`Fetcher`] makes a real HTTP request; the one test that does is
//! `#[ignore]`d (see `tests/live_download.rs`).

use std::io::{Read, Write};

use crate::error::Error;

/// Fetches the body of a URL, streaming it into `dest` and reporting
/// progress as it goes.
pub trait Fetcher: Send + Sync {
    /// Stream the full body of `url` into `dest`, calling
    /// `on_progress(bytes_written_so_far, total_bytes_if_known)` after each
    /// chunk. Returns the total number of bytes written on success.
    fn fetch(
        &self,
        url: &str,
        dest: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<u64, Error>;
}

/// Real network fetcher, backed by `ureq` (blocking, TLS via `rustls`).
#[derive(Debug, Default, Clone, Copy)]
pub struct UreqFetcher;

impl Fetcher for UreqFetcher {
    fn fetch(
        &self,
        url: &str,
        dest: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<u64, Error> {
        let resp = ureq::get(url).call().map_err(|source| Error::Http {
            url: url.to_string(),
            source: Box::new(source),
        })?;
        let total = resp
            .header("Content-Length")
            .and_then(|s| s.parse::<u64>().ok());
        let mut reader = resp.into_reader();

        let mut buf = [0u8; 64 * 1024];
        let mut written = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|source| Error::Read {
                url: url.to_string(),
                source,
            })?;
            if n == 0 {
                break;
            }
            dest.write_all(&buf[..n]).map_err(|source| Error::Write {
                url: url.to_string(),
                source,
            })?;
            written += n as u64;
            on_progress(written, total);
        }
        Ok(written)
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    //! A fake [`Fetcher`] for unit tests elsewhere in this crate: serves
    //! canned bytes (or a canned error) per URL, from memory, with no
    //! network I/O at all.

    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::Mutex;

    use super::Fetcher;
    use crate::error::Error;

    /// What [`FakeFetcher`] does for a given URL.
    pub enum Canned {
        /// Return these exact bytes.
        Bytes(Vec<u8>),
        /// Fail the request as if the network itself were down.
        Error,
    }

    /// In-memory [`Fetcher`] for tests. Records every URL requested so
    /// tests can assert on idempotency (e.g. "the second `ensure_models`
    /// call fetched nothing").
    #[derive(Default)]
    pub struct FakeFetcher {
        responses: HashMap<String, Canned>,
        requested: Mutex<Vec<String>>,
    }

    impl FakeFetcher {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_bytes(mut self, url: impl Into<String>, bytes: Vec<u8>) -> Self {
            self.responses.insert(url.into(), Canned::Bytes(bytes));
            self
        }

        pub fn with_error(mut self, url: impl Into<String>) -> Self {
            self.responses.insert(url.into(), Canned::Error);
            self
        }

        /// URLs actually requested via [`Fetcher::fetch`], in order.
        pub fn requested(&self) -> Vec<String> {
            self.requested.lock().unwrap().clone()
        }
    }

    impl Fetcher for FakeFetcher {
        fn fetch(
            &self,
            url: &str,
            dest: &mut dyn Write,
            on_progress: &mut dyn FnMut(u64, Option<u64>),
        ) -> Result<u64, Error> {
            self.requested.lock().unwrap().push(url.to_string());
            match self.responses.get(url) {
                Some(Canned::Bytes(bytes)) => {
                    let total = Some(bytes.len() as u64);
                    // Write in chunks so progress reporting is exercised too.
                    let mut written = 0u64;
                    for chunk in bytes.chunks(7) {
                        dest.write_all(chunk).map_err(|source| Error::Write {
                            url: url.to_string(),
                            source,
                        })?;
                        written += chunk.len() as u64;
                        on_progress(written, total);
                    }
                    Ok(written)
                }
                Some(Canned::Error) | None => Err(Error::FetchFailed {
                    url: url.to_string(),
                    reason: "fake fetcher: simulated failure (no canned response, or an \
                              explicit with_error() for this URL)"
                        .to_string(),
                }),
            }
        }
    }
}
