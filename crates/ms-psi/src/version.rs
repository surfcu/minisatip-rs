//! Per-table version tracking.

use std::collections::HashMap;

use crate::header::SectionHeader;

/// How an observed version relates to the previously seen one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionStatus {
    /// First version seen for this table.
    New,
    /// Same version as the previous section.
    Same,
    /// A different version than the previous section.
    Changed,
}

/// Tracks the last version seen per `(table_id, table_id_extension)`.
#[derive(Debug, Default)]
pub struct VersionTracker {
    versions: HashMap<(u8, u16), u8>,
}

impl VersionTracker {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a version and reports how it relates to the previous one.
    pub fn observe(
        &mut self,
        table_id: u8,
        table_id_extension: u16,
        version_number: u8,
    ) -> VersionStatus {
        match self
            .versions
            .insert((table_id, table_id_extension), version_number)
        {
            None => VersionStatus::New,
            Some(previous) if previous == version_number => VersionStatus::Same,
            Some(_) => VersionStatus::Changed,
        }
    }

    /// Records the version carried by a parsed section header.
    pub fn observe_header(&mut self, header: &SectionHeader) -> VersionStatus {
        self.observe(
            header.table_id,
            header.table_id_extension,
            header.version_number,
        )
    }

    /// Forgets the stored version for one table.
    pub fn forget(&mut self, table_id: u8, table_id_extension: u16) -> bool {
        self.versions
            .remove(&(table_id, table_id_extension))
            .is_some()
    }

    /// Forgets every stored version.
    pub fn clear(&mut self) {
        self.versions.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_new_same_and_changed() {
        let mut tracker = VersionTracker::new();
        assert_eq!(tracker.observe(0x00, 1, 3), VersionStatus::New);
        assert_eq!(tracker.observe(0x00, 1, 3), VersionStatus::Same);
        assert_eq!(tracker.observe(0x00, 1, 4), VersionStatus::Changed);
        assert_eq!(tracker.observe(0x00, 1, 4), VersionStatus::Same);
    }

    #[test]
    fn tracks_tables_independently() {
        let mut tracker = VersionTracker::new();
        assert_eq!(tracker.observe(0x00, 1, 3), VersionStatus::New);
        assert_eq!(tracker.observe(0x02, 1, 3), VersionStatus::New);
        assert_eq!(tracker.observe(0x02, 2, 7), VersionStatus::New);
        assert_eq!(tracker.observe(0x00, 1, 3), VersionStatus::Same);
        assert_eq!(tracker.observe(0x02, 1, 4), VersionStatus::Changed);
        assert!(tracker.forget(0x02, 1));
        assert!(!tracker.forget(0x02, 1));
        assert_eq!(tracker.observe(0x02, 1, 4), VersionStatus::New);
        tracker.clear();
        assert_eq!(tracker.observe(0x00, 1, 3), VersionStatus::New);
    }
}
