//! Human-readable progress reporting for downloads.

use std::collections::HashMap;
use std::time::Duration;

/// A single step in a model download/install, reported to whatever
/// progress callback [`crate::ensure_models`] (or its testable `_at`
/// variant) was given.
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// A file's download is beginning.
    Starting {
        /// Which bundle this file belongs to (`"stt"` or `"tts"`).
        bundle: &'static str,
        /// File (or archive) name being fetched.
        file: String,
        /// Total size if the server reported `Content-Length`.
        total_bytes: Option<u64>,
    },
    /// Bytes have been written for a file currently downloading.
    Progress {
        /// Which bundle this file belongs to.
        bundle: &'static str,
        /// File (or archive) name being fetched.
        file: String,
        /// Bytes written so far.
        bytes: u64,
        /// Total size if known.
        total_bytes: Option<u64>,
    },
    /// A file was already present with the correct hash; nothing was
    /// downloaded.
    Skipped {
        /// Which bundle this file belongs to.
        bundle: &'static str,
        /// File (or bundle directory) that was already installed.
        file: String,
    },
    /// A file finished downloading and verifying successfully.
    Finished {
        /// Which bundle this file belongs to.
        bundle: &'static str,
        /// File (or archive) name that finished.
        file: String,
        /// Total bytes written.
        bytes: u64,
        /// How long the download took.
        elapsed: Duration,
    },
}

/// Render a byte count as e.g. `128.4 MB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A progress callback that prints a live-updating line per file to
/// stderr, throttled to roughly one update per megabyte per file -- so a
/// multi-minute download of a 129/166 MB bundle prints steadily instead of
/// either spamming the terminal or looking like a silent hang.
pub fn default_printer() -> impl FnMut(ProgressEvent) {
    const THROTTLE_BYTES: u64 = 1024 * 1024;
    let mut last_printed: HashMap<String, u64> = HashMap::new();
    move |event: ProgressEvent| match event {
        ProgressEvent::Starting {
            bundle,
            file,
            total_bytes,
        } => {
            last_printed.insert(file.clone(), 0);
            match total_bytes {
                Some(t) => eprintln!("[{bundle}] downloading {file} ({})...", human_bytes(t)),
                None => eprintln!("[{bundle}] downloading {file}..."),
            }
        }
        ProgressEvent::Progress {
            bundle,
            file,
            bytes,
            total_bytes,
        } => {
            let last = last_printed.get(&file).copied().unwrap_or(0);
            if bytes.saturating_sub(last) < THROTTLE_BYTES {
                return;
            }
            last_printed.insert(file.clone(), bytes);
            match total_bytes {
                Some(t) if t > 0 => {
                    let pct = (bytes as f64 / t as f64 * 100.0).min(100.0);
                    eprint!(
                        "\r[{bundle}] {file}: {} / {} ({pct:.0}%)      ",
                        human_bytes(bytes),
                        human_bytes(t)
                    );
                }
                _ => eprint!("\r[{bundle}] {file}: {}      ", human_bytes(bytes)),
            }
        }
        ProgressEvent::Skipped { bundle, file } => {
            eprintln!("[{bundle}] {file}: already present, verified -- skipping");
        }
        ProgressEvent::Finished {
            bundle,
            file,
            bytes,
            elapsed,
        } => {
            eprintln!(
                "\r[{bundle}] {file}: {} verified in {:.1}s                    ",
                human_bytes(bytes),
                elapsed.as_secs_f64()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formats_reasonably() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(129 * 1024 * 1024), "129.0 MB");
    }
}
