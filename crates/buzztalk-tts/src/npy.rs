//! Minimal reader for the one `.npy` array this bundle ships: the learned
//! voice BOS embedding, always a little-endian C-order float32 array.
//!
//! Ported from Block's `buzz-voice` crate (`src/pocket_april.rs`,
//! Apache-2.0) — see the crate-level attribution in `lib.rs`.

use std::fs;
use std::path::Path;

use crate::error::{Error, Result};

/// Read a little-endian, C-order float32 NumPy `.npy` array as a flat
/// `Vec<f32>`, ignoring its declared shape (the caller already knows what
/// shape to expect from `bundle.json`).
pub fn read_npy_f32(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let npy_err = |reason: String| Error::Npy {
        path: path.to_path_buf(),
        reason,
    };

    if bytes.len() < 10 || &bytes[..6] != b"\x93NUMPY" {
        return Err(npy_err("not a NumPy array (bad magic)".to_string()));
    }
    let major = bytes[6];
    let header_len_bytes = match major {
        1 => 2,
        2 | 3 => 4,
        other => return Err(npy_err(format!("unsupported NumPy version {other}"))),
    };
    let header_start = 8 + header_len_bytes;
    if bytes.len() < header_start {
        return Err(npy_err("truncated NumPy header".to_string()));
    }
    let header_len = if header_len_bytes == 2 {
        u16::from_le_bytes([bytes[8], bytes[9]]) as usize
    } else {
        u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize
    };
    let data_start = header_start
        .checked_add(header_len)
        .ok_or_else(|| npy_err("NumPy header length overflow".to_string()))?;
    if data_start > bytes.len() {
        return Err(npy_err("truncated NumPy data".to_string()));
    }
    let header = std::str::from_utf8(&bytes[header_start..data_start])
        .map_err(|err| npy_err(format!("invalid NumPy header: {err}")))?;
    if !(header.contains("'descr': '<f4'") || header.contains("\"descr\": \"<f4\""))
        || header.contains("fortran_order': True")
        || header.contains("fortran_order\": true")
    {
        return Err(npy_err(
            "must be a little-endian, C-order float32 array".to_string(),
        ));
    }
    let data = &bytes[data_start..];
    if !data.len().is_multiple_of(4) {
        return Err(npy_err("misaligned float32 data".to_string()));
    }
    Ok(data
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_npy_f32(values: &[f32]) -> Vec<u8> {
        let header = format!(
            "{{'descr': '<f4', 'fortran_order': False, 'shape': ({},), }}",
            values.len()
        );
        let mut header_bytes = header.into_bytes();
        // Pad so (10 + header_len) is a multiple of 64, NumPy-style, then
        // terminate with a newline.
        header_bytes.push(b'\n');
        while (10 + header_bytes.len()) % 64 != 0 {
            header_bytes.insert(header_bytes.len() - 1, b' ');
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x93NUMPY");
        bytes.push(1); // major
        bytes.push(0); // minor
        bytes.extend_from_slice(&(header_bytes.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&header_bytes);
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn round_trips_a_float32_array() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.npy");
        let values = vec![1.0_f32, -2.5, 0.0, 3.25, f32::NAN];
        fs::write(&path, write_npy_f32(&values)).unwrap();

        let read = read_npy_f32(&path).unwrap();
        assert_eq!(read.len(), values.len());
        assert_eq!(read[0], 1.0);
        assert_eq!(read[1], -2.5);
        assert_eq!(read[3], 3.25);
        assert!(read[4].is_nan());
    }

    #[test]
    fn rejects_bad_magic() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bad.npy");
        fs::write(&path, b"not an npy file at all, way too short").unwrap();
        let err = read_npy_f32(&path).unwrap_err();
        assert!(matches!(err, Error::Npy { .. }));
    }

    #[test]
    fn rejects_missing_file() {
        let err = read_npy_f32(Path::new("/nonexistent/file.npy")).unwrap_err();
        assert!(matches!(err, Error::Io { .. }));
    }
}
