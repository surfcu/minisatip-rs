//! Stored CI+ authentication data (`get_authdata`/`write_authdata`,
//! ca.cpp:1623-1727).

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::MAX_PAIRS;
use super::ffi::wipe;

/// One `host_id + DHSK + AKH` entry.
pub const RECORD_LEN: usize = 8 + 256 + 32;

/// The authentication record kept on disk. Key material is zeroed on drop and
/// never shown by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct Record {
    pub host_id: [u8; 8],
    pub dhsk: [u8; 256],
    pub akh: [u8; 32],
}

impl Record {
    fn as_bytes(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[..8].copy_from_slice(&self.host_id);
        out[8..8 + 256].copy_from_slice(&self.dhsk);
        out[8 + 256..].copy_from_slice(&self.akh);
        out
    }

    fn from_bytes(bytes: &[u8; RECORD_LEN]) -> Self {
        let mut record = Self {
            host_id: [0u8; 8],
            dhsk: [0u8; 256],
            akh: [0u8; 32],
        };
        record.host_id.copy_from_slice(&bytes[..8]);
        record.dhsk.copy_from_slice(&bytes[8..8 + 256]);
        record.akh.copy_from_slice(&bytes[8 + 256..]);
        record
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        wipe(&mut self.host_id);
        wipe(&mut self.dhsk);
        wipe(&mut self.akh);
    }
}

impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("host_id", &self.host_id)
            .field("dhsk", &"[redacted]")
            .field("akh", &"[redacted]")
            .finish()
    }
}

/// `get_authdata`: record `index` (0-based), `None` past `MAX_PAIRS` or when
/// the file or a record up to `index` is missing, exactly like C++.
pub fn get_authdata(path: &Path, index: usize) -> Option<Record> {
    if index > MAX_PAIRS {
        return None;
    }
    let Ok(mut file) = File::open(path) else {
        ms_log::log!("cannot open {}", path.display());
        return None;
    };
    let mut record = None;
    for _ in 0..=index {
        let mut buf = [0u8; RECORD_LEN];
        let read = file.read_exact(&mut buf);
        if read.is_err() {
            wipe(&mut buf);
            ms_log::log!("cannot read auth_data");
            return None;
        }
        record = Some(Record::from_bytes(&buf));
        wipe(&mut buf);
    }
    record
}

/// `write_authdata`: keeps at most four records, newest first, mode 0600. An
/// existing record with the same AKH is not written again.
pub fn write_authdata(path: &Path, record: &Record) -> bool {
    let mut records = Vec::new();
    for index in 0..MAX_PAIRS {
        let Some(old) = get_authdata(path, index) else {
            break;
        };
        records.push(old);
    }

    if records.iter().any(|known| known.akh == record.akh) {
        ms_log::log!("data already stored");
        return true;
    }
    ms_log::log!("got {} entries", records.len());

    let mut file = match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => {
            ms_log::log!(
                "cannot open {} for writing - authdata not stored",
                path.display()
            );
            return false;
        }
    };

    let mut bytes = record.as_bytes();
    let written = file.write_all(&bytes);
    wipe(&mut bytes);
    if written.is_err() {
        ms_log::log!("error in write");
        return false;
    }

    records.truncate(3);
    for old in &records {
        let mut bytes = old.as_bytes();
        let written = file.write_all(&bytes);
        wipe(&mut bytes);
        if written.is_err() {
            ms_log::log!("error in write");
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        std::env::temp_dir().join(format!(
            "ms-ciplus-auth-{}-{tag}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn record(seed: u8) -> Record {
        Record {
            host_id: [seed; 8],
            dhsk: [seed; 256],
            akh: [seed; 32],
        }
    }

    #[test]
    fn round_trip() {
        let path = temp_path("round-trip");
        let written = record(7);
        assert!(write_authdata(&path, &written));
        let read = get_authdata(&path, 0).expect("record 0");
        assert_eq!(read, written);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn duplicate_akh_is_not_written() {
        let path = temp_path("duplicate");
        let written = record(3);
        assert!(write_authdata(&path, &written));
        assert!(write_authdata(&path, &written));
        assert!(get_authdata(&path, 1).is_none());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), RECORD_LEN as u64);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn keeps_four_records_newest_first() {
        let path = temp_path("rotation");
        for seed in 0..6 {
            assert!(write_authdata(&path, &record(seed)));
        }
        for (index, seed) in [(0, 5), (1, 4), (2, 3), (3, 2)] {
            assert_eq!(
                get_authdata(&path, index),
                Some(record(seed)),
                "index {index}"
            );
        }
        assert!(get_authdata(&path, 4).is_none());
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            (RECORD_LEN * 4) as u64
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn reading_stops_after_the_wanted_record() {
        let path = temp_path("prefix");
        let mut data = record(9).as_bytes().to_vec();
        data.extend_from_slice(&[0u8; RECORD_LEN - 1]);
        std::fs::write(&path, data).unwrap();
        assert_eq!(get_authdata(&path, 0), Some(record(9)));
        assert!(get_authdata(&path, 1).is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn short_file_returns_none() {
        let path = temp_path("short");
        std::fs::write(&path, [0u8; RECORD_LEN - 1]).unwrap();
        assert!(get_authdata(&path, 0).is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn file_mode_is_0600() {
        let path = temp_path("mode");
        assert!(write_authdata(&path, &record(1)));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn index_past_max_pairs_is_none() {
        let path = temp_path("limit");
        assert!(write_authdata(&path, &record(1)));
        assert!(get_authdata(&path, MAX_PAIRS).is_none());
        assert!(get_authdata(&path, MAX_PAIRS + 1).is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn missing_file_is_none() {
        let path = temp_path("missing");
        assert!(get_authdata(&path, 0).is_none());
    }
}
