//! Read-only reporting: what's on disk, what's missing, how big it is.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::install::stt_dir_is_complete;
use crate::manifest::{STT_DIR_NAME, STT_REQUIRED_FILES, TTS_DIR_NAME, TTS_FILES};
use crate::progress::human_bytes;

/// Presence/completeness report for one model bundle.
#[derive(Debug, Clone)]
pub struct BundleStatus {
    /// Human-readable bundle name (`"stt"` or `"tts"`).
    pub name: &'static str,
    /// Where this bundle would live on disk.
    pub dir: PathBuf,
    /// `true` if every required file is present (and, for the STT bundle,
    /// the install is verified via its source-hash sentinel).
    pub complete: bool,
    /// File names found present.
    pub present_files: Vec<String>,
    /// File names expected but missing.
    pub missing_files: Vec<String>,
    /// Total bytes currently on disk under `dir` (0 if `dir` doesn't exist).
    pub bytes_on_disk: u64,
}

/// Combined status of both model bundles.
#[derive(Debug, Clone)]
pub struct ModelStatus {
    /// The resolved parent models directory both bundles live under.
    pub models_dir: PathBuf,
    /// STT (Parakeet) bundle status.
    pub stt: BundleStatus,
    /// TTS (Pocket TTS) bundle status.
    pub tts: BundleStatus,
}

impl ModelStatus {
    /// `true` if both bundles are fully present and verified.
    pub fn all_present(&self) -> bool {
        self.stt.complete && self.tts.complete
    }

    /// Total bytes on disk across both bundles.
    pub fn total_bytes_on_disk(&self) -> u64 {
        self.stt.bytes_on_disk + self.tts.bytes_on_disk
    }
}

impl fmt::Display for ModelStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "models directory: {}", self.models_dir.display())?;
        for bundle in [&self.stt, &self.tts] {
            let state = if bundle.complete {
                "present"
            } else {
                "MISSING/INCOMPLETE"
            };
            writeln!(
                f,
                "  {} [{}]: {} ({} on disk)",
                bundle.name,
                state,
                bundle.dir.display(),
                human_bytes(bundle.bytes_on_disk)
            )?;
            if !bundle.missing_files.is_empty() {
                writeln!(f, "    missing: {}", bundle.missing_files.join(", "))?;
            }
        }
        writeln!(
            f,
            "  total on disk: {}",
            human_bytes(self.total_bytes_on_disk())
        )?;
        Ok(())
    }
}

/// Recursively sum file sizes under `dir`. Returns 0 if `dir` doesn't
/// exist. Errors reading individual entries are treated as "0 bytes for
/// that entry" rather than failing the whole report -- `status()` is a
/// best-effort diagnostic, not something that should itself fail because
/// of a permissions quirk on one file.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = entry.metadata() {
            if meta.is_dir() {
                total += dir_size(&path);
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Compute [`ModelStatus`] for bundles rooted at `base_dir`.
pub fn status_at(base_dir: &Path) -> ModelStatus {
    let stt_dir = base_dir.join(STT_DIR_NAME);
    let mut stt_present = Vec::new();
    let mut stt_missing = Vec::new();
    for name in STT_REQUIRED_FILES {
        if stt_dir.join(name).is_file() {
            stt_present.push((*name).to_string());
        } else {
            stt_missing.push((*name).to_string());
        }
    }
    let stt = BundleStatus {
        name: "stt",
        dir: stt_dir.clone(),
        complete: stt_dir_is_complete(&stt_dir),
        present_files: stt_present,
        missing_files: stt_missing,
        bytes_on_disk: dir_size(&stt_dir),
    };

    let tts_dir = base_dir.join(TTS_DIR_NAME);
    let mut tts_present = Vec::new();
    let mut tts_missing = Vec::new();
    for spec in TTS_FILES {
        if tts_dir.join(spec.file_name).is_file() {
            tts_present.push(spec.file_name.to_string());
        } else {
            tts_missing.push(spec.file_name.to_string());
        }
    }
    let tts = BundleStatus {
        name: "tts",
        dir: tts_dir.clone(),
        complete: tts_missing.is_empty() && tts_dir.is_dir(),
        present_files: tts_present,
        missing_files: tts_missing,
        bytes_on_disk: dir_size(&tts_dir),
    };

    ModelStatus {
        models_dir: base_dir.to_path_buf(),
        stt,
        tts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn status_reports_absent_when_nothing_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let status = status_at(dir.path());
        assert!(!status.stt.complete);
        assert!(!status.tts.complete);
        assert_eq!(status.total_bytes_on_disk(), 0);
        assert!(!status.all_present());
        assert_eq!(status.stt.missing_files.len(), STT_REQUIRED_FILES.len());
        assert_eq!(status.tts.missing_files.len(), TTS_FILES.len());
    }

    #[test]
    fn status_reports_partial_tts_install() {
        let dir = tempfile::tempdir().unwrap();
        let tts_dir = dir.path().join(TTS_DIR_NAME);
        fs::create_dir_all(&tts_dir).unwrap();
        // Install only the first file.
        fs::write(tts_dir.join(TTS_FILES[0].file_name), b"x").unwrap();

        let status = status_at(dir.path());
        assert!(!status.tts.complete);
        assert_eq!(status.tts.present_files.len(), 1);
        assert_eq!(status.tts.missing_files.len(), TTS_FILES.len() - 1);
        assert_eq!(status.tts.bytes_on_disk, 1);
    }

    #[test]
    fn status_reports_present_when_fully_installed() {
        let dir = tempfile::tempdir().unwrap();

        let tts_dir = dir.path().join(TTS_DIR_NAME);
        fs::create_dir_all(&tts_dir).unwrap();
        for spec in TTS_FILES {
            fs::write(tts_dir.join(spec.file_name), b"payload").unwrap();
        }

        let stt_dir = dir.path().join(STT_DIR_NAME);
        fs::create_dir_all(&stt_dir).unwrap();
        for name in STT_REQUIRED_FILES {
            fs::write(stt_dir.join(name), b"payload").unwrap();
        }
        fs::write(
            stt_dir.join(crate::install::STT_SOURCE_SHA256_FILE),
            crate::manifest::STT_ARCHIVE_SHA256,
        )
        .unwrap();

        let status = status_at(dir.path());
        assert!(status.tts.complete);
        assert!(status.stt.complete);
        assert!(status.all_present());
        assert!(status.total_bytes_on_disk() > 0);
        assert!(status.stt.missing_files.is_empty());
        assert!(status.tts.missing_files.is_empty());
    }
}
