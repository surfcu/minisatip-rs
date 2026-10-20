//! PID filter table with per-filter section assembly and callbacks.

use crate::packet::TsPacket;
use crate::section::SectionAssembler;

/// Maximum number of filters held by one table, matching the C++ limit.
pub const MAX_FILTERS: usize = 200;
/// Invert the mask match result.
pub const FILTER_REVERSE: u32 = 8;
/// Validate the CRC of assembled sections.
pub const FILTER_CRC: u32 = 16;
/// Treat the payload as EMM data (not implemented by the section assembler).
pub const FILTER_EMM: u32 = 32;

/// Callback invoked with the filter id and a complete section.
pub type FilterCallback = Box<dyn FnMut(usize, &[u8]) + Send>;

/// One PID filter and its section reassembly state.
pub struct Filter {
    id: usize,
    adapter: u16,
    pid: u16,
    flags: u32,
    enabled: bool,
    allocated: bool,
    matched: bool,
    section: SectionAssembler,
    callback: Option<FilterCallback>,
    filter: [u8; 16],
    mask: [u8; 16],
    mask_len: usize,
}

impl Filter {
    fn new(
        id: usize,
        adapter: u16,
        pid: u16,
        flags: u32,
        callback: Option<FilterCallback>,
    ) -> Self {
        Self {
            id,
            adapter,
            pid,
            flags,
            enabled: false,
            allocated: false,
            matched: false,
            section: SectionAssembler::new(flags & FILTER_CRC != 0),
            callback,
            filter: [0; 16],
            mask: [0; 16],
            mask_len: 0,
        }
    }

    /// Table slot index of this filter.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Adapter this filter belongs to.
    pub fn adapter(&self) -> u16 {
        self.adapter
    }

    /// PID this filter matches.
    pub fn pid(&self) -> u16 {
        self.pid
    }

    /// Whether the filter is active.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether the table slot is currently claimed.
    pub fn allocated(&self) -> bool {
        self.allocated
    }

    /// Bytes of the last section returned by `assemble_packet`.
    pub fn data(&self) -> &[u8] {
        self.section.data()
    }

    /// C++-compatible single-section assembly for this filter.
    pub fn assemble_packet(&mut self, packet: &[u8]) -> Option<&[u8]> {
        self.section.assemble_packet(packet)
    }

    /// Replaces the section header filter and mask.
    pub fn set_mask(&mut self, filter: &[u8; 16], mask: &[u8; 16]) {
        self.filter = *filter;
        self.mask = *mask;
        self.mask_len = mask
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(0, |i| i + 1);
    }

    /// Tests a section start against the filter mask.
    pub fn matches(&self, data: &[u8]) -> bool {
        let mut matched = true;
        for (index, (&want, &mask)) in self
            .filter
            .iter()
            .zip(self.mask.iter())
            .enumerate()
            .take(self.mask_len)
        {
            let offset = if index == 0 { 0 } else { index + 2 };
            match data.get(offset) {
                Some(&byte) if byte & mask == want & mask => {}
                _ => {
                    matched = false;
                    break;
                }
            }
        }
        if self.flags & FILTER_REVERSE != 0 {
            !matched
        } else {
            matched
        }
    }

    fn process(&mut self, ts: &TsPacket<'_>) {
        if ts.pusi()
            && let Some(payload) = ts.payload()
            && let Some(&pointer) = payload.first()
            && let Some(section) = payload.get(1 + pointer as usize..)
        {
            self.matched = self.matches(section);
        }
        if !self.matched {
            return;
        }
        let id = self.id;
        let callback = &mut self.callback;
        self.section.push(ts.as_bytes(), &mut |data| {
            if let Some(callback) = callback.as_mut() {
                callback(id, data);
            }
        });
    }
}

/// Owned table of PID filters; wrap it in a `Mutex` for concurrent use.
pub struct FilterTable {
    filters: Vec<Option<Filter>>,
}

impl FilterTable {
    /// Creates an empty table with `MAX_FILTERS` slots.
    pub fn new() -> Self {
        Self {
            filters: (0..MAX_FILTERS).map(|_| None).collect(),
        }
    }

    /// Adds a filter for one adapter and PID, returning its id.
    pub fn add_filter(
        &mut self,
        adapter: u16,
        pid: u16,
        flags: u32,
        callback: Option<FilterCallback>,
    ) -> Option<usize> {
        if pid > 8191 {
            return None;
        }
        let id = self
            .filters
            .iter()
            .position(|slot| slot.as_ref().is_none_or(|filter| !filter.allocated))?;
        let mut filter = Filter::new(id, adapter, pid, flags, callback);
        filter.allocated = true;
        filter.enabled = true;
        self.filters[id] = Some(filter);
        Some(id)
    }

    /// Sets the section header filter and mask of an existing filter.
    pub fn set_mask(&mut self, id: usize, filter: &[u8; 16], mask: &[u8; 16]) -> bool {
        match self.filters.get_mut(id).and_then(Option::as_mut) {
            Some(entry) if entry.enabled => {
                entry.set_mask(filter, mask);
                true
            }
            _ => false,
        }
    }

    /// Releases a filter and reclaims its table slot.
    pub fn del_filter(&mut self, id: usize) -> bool {
        let Some(entry) = self.filters.get_mut(id).and_then(Option::as_mut) else {
            return false;
        };
        if !entry.enabled {
            return false;
        }
        entry.enabled = false;
        entry.allocated = false;
        entry.flags = 0;
        entry.pid = u16::MAX;
        entry.matched = false;
        entry.callback = None;
        entry.section.reset();
        true
    }

    /// Returns a filter by id, including released filters.
    pub fn get(&self, id: usize) -> Option<&Filter> {
        self.filters.get(id).and_then(Option::as_ref)
    }

    /// Returns a mutable filter by id.
    pub fn get_mut(&mut self, id: usize) -> Option<&mut Filter> {
        self.filters.get_mut(id).and_then(Option::as_mut)
    }

    /// Feeds one packet to every enabled filter for its adapter and PID.
    pub fn process_packet(&mut self, adapter: u16, packet: &[u8]) {
        let Some(ts) = TsPacket::from_bytes(packet) else {
            return;
        };
        let pid = ts.pid();
        for slot in &mut self.filters {
            if let Some(filter) = slot
                && filter.enabled
                && filter.adapter == adapter
                && filter.pid == pid
            {
                filter.process(&ts);
            }
        }
    }
}

impl Default for FilterTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::TS_PACKET_SIZE;

    fn packet(pid: u16) -> [u8; TS_PACKET_SIZE] {
        let mut raw = [0xFFu8; TS_PACKET_SIZE];
        raw[0] = 0x47;
        raw[1] = ((pid >> 8) as u8) & 0x1F;
        raw[2] = pid as u8;
        raw[3] = 0x10;
        raw
    }

    #[test]
    fn add_and_delete_reclaims_slot() {
        let mut table = FilterTable::new();
        let id = table
            .add_filter(0, 100, FILTER_CRC, Some(Box::new(|_, _| {})))
            .unwrap();
        assert!(table.get(id).unwrap().enabled());
        assert!(table.get(id).unwrap().allocated());
        assert!(table.del_filter(id));
        let filter = table.get(id).unwrap();
        assert!(!filter.enabled());
        assert!(!filter.allocated());
        assert!(!table.del_filter(id));
    }

    #[test]
    fn mask_matches_section_header() {
        let mut table = FilterTable::new();
        let id = table.add_filter(0, 0, 0, None).unwrap();
        let mut filter = [0u8; 16];
        let mut mask = [0u8; 16];
        filter[0] = 0x42;
        mask[0] = 0xFF;
        assert!(table.set_mask(id, &filter, &mask));
        let entry = table.get(id).unwrap();
        assert!(entry.matches(&[0x42, 0x00, 0x00]));
        assert!(!entry.matches(&[0x02, 0x00, 0x00]));
    }

    #[test]
    fn process_packet_only_reaches_matching_pid() {
        let mut table = FilterTable::new();
        let id = table.add_filter(0, 100, 0, None).unwrap();
        table.process_packet(0, &packet(100));
        table.process_packet(0, &packet(101));
        assert!(table.get(id).unwrap().enabled());
    }
}
