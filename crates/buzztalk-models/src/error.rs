//! Errors surfaced by `buzztalk-models`.

use std::path::PathBuf;

/// Errors that can occur while acquiring or inspecting model bundles.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Could not create a directory needed for staging or installation.
    #[error("could not create directory {path}: {source}")]
    CreateDir {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Could not open a file for writing (e.g. a download staging file).
    #[error("could not open {path} for writing: {source}")]
    OpenFile {
        /// Path that could not be opened.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The HTTP request itself failed (DNS, TLS, connection, non-2xx status).
    #[error("request to {url} failed: {source}")]
    Http {
        /// URL that was requested.
        url: String,
        /// Underlying `ureq` error.
        #[source]
        source: Box<ureq::Error>,
    },

    /// A fetch failed for a reason not tied to the real HTTP client (used
    /// by the in-memory test fetcher, and as a catch-all for fetcher
    /// implementations other than [`crate::fetch::UreqFetcher`]).
    #[error("fetch failed for {url}: {reason}")]
    FetchFailed {
        /// URL that was requested.
        url: String,
        /// Human-readable reason.
        reason: String,
    },

    /// Reading the response body failed partway through.
    #[error("reading response body from {url} failed: {source}")]
    Read {
        /// URL being downloaded.
        url: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Writing downloaded bytes to the staging file failed.
    #[error("writing downloaded data for {url} failed: {source}")]
    Write {
        /// URL being downloaded.
        url: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The downloaded content's SHA-256 does not match the pinned hash.
    ///
    /// This is the integrity gate: a mismatch means either a corrupted
    /// transfer or a compromised mirror, and in either case the bytes must
    /// not be installed. The half-downloaded/staged file is discarded, not
    /// moved into place.
    #[error(
        "hash mismatch for {url}: expected {expected}, got {actual} -- refusing to install \
         (corrupted download or compromised mirror)"
    )]
    HashMismatch {
        /// URL the bytes came from.
        url: String,
        /// Expected (pinned) SHA-256 hex digest.
        expected: String,
        /// Actual SHA-256 hex digest of the downloaded bytes.
        actual: String,
    },

    /// A verified staged file could not be moved into its final location.
    #[error("could not install {path}: {source}")]
    Persist {
        /// Final destination path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The downloaded archive could not be extracted.
    #[error("could not extract archive from {url}: {reason}")]
    Extract {
        /// URL the archive came from.
        url: String,
        /// Human-readable reason extraction failed.
        reason: String,
    },

    /// A verified, extracted directory could not be moved into its final
    /// location.
    #[error("could not move {from} into place at {to}: {source}")]
    Rename {
        /// Staged source directory.
        from: PathBuf,
        /// Final destination directory.
        to: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A stale/incomplete install directory could not be removed to make
    /// way for a fresh atomic install.
    #[error("could not remove stale directory {path}: {source}")]
    RemoveDir {
        /// Directory that could not be removed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A miscellaneous I/O error not covered by a more specific variant.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Convenience alias, mirroring the pattern used by `buzztalk-tts` and
/// `buzztalk-stt`.
pub type Result<T> = std::result::Result<T, Error>;
