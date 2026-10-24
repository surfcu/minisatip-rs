//! CI+ credential elements (`element_*`, ca.cpp:1421-1604).

use std::fmt;

use super::MAX_ELEMENTS;
use super::ffi::wipe;

/// `#define DEFAULT_LOG LOG_DVBCA` (ca.cpp:52).
const DEFAULT_LOG: u32 = ms_log::LOG_DVBCA;

/// `datatype_sizes` (ca.cpp:93): fixed lengths; 0 means any length.
pub const DATATYPE_SIZES: [u32; MAX_ELEMENTS] = [
    0, 0, 0, 0, 0, 8, 8, 0, 0, 0, 0, 0, // 0-11
    32, 256, 256, 0, 0, 256, 256, 32, // 12-19
    8, 8, 32, 32, 0, 8, 2, 32, 1, 32, // 20-29
    1, 0, 32, 0, 1, 1, 0, 1, 1, 0, 1, // 30-40
    1, 1, 0, 0, 0, 0, 0, 0, 1, 1, 1, // 41-51
    4, 0, 1, 0, // 52-55
];

/// `struct element elements[MAX_ELEMENTS]`: `None` is an invalid element.
pub struct Elements {
    slots: [Option<Vec<u8>>; MAX_ELEMENTS],
}

impl Default for Elements {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
        }
    }
}

impl Elements {
    pub fn new() -> Self {
        Self::default()
    }

    /// `element_set`: stores data for id 1..=55 with the size check.
    pub fn set(&mut self, id: usize, data: &[u8]) -> bool {
        if id == 0 || id >= MAX_ELEMENTS {
            ms_log::log!("element_get: invalid id");
            return false;
        }
        let expected = DATATYPE_SIZES[id];
        if expected != 0 && expected != data.len() as u32 {
            ms_log::log!("size {} of datatype_id {} doesn't match", data.len(), id);
            return false;
        }
        self.slots[id] = Some(data.to_vec());
        ms_log::logm!("_element_set_ stored {} with len {}", id, data.len());
        true
    }

    /// `element_invalidate`: drops and zeroes the stored bytes.
    pub fn invalidate(&mut self, id: usize) {
        if id == 0 || id >= MAX_ELEMENTS {
            return;
        }
        if let Some(mut data) = self.slots[id].take() {
            wipe(&mut data);
        }
    }

    /// `element_init`: clears every element.
    pub fn init(&mut self) {
        for id in 1..MAX_ELEMENTS {
            self.invalidate(id);
        }
    }

    /// `element_valid`: true when the element holds data.
    pub fn valid(&self, id: usize) -> bool {
        id != 0 && id < MAX_ELEMENTS && self.slots[id].is_some()
    }

    /// `element_get_buf`: the stored bytes, only while valid.
    pub fn get(&self, id: usize) -> Option<&[u8]> {
        if id == 0 || id >= MAX_ELEMENTS {
            return None;
        }
        self.slots[id].as_deref()
    }

    /// `element_get_req`: appends id, u16 BE length and data to `out`.
    pub fn get_req(&self, id: usize, out: &mut Vec<u8>) -> bool {
        let Some(data) = self.get(id) else {
            ms_log::log!("can not get element {}", id);
            return false;
        };
        if data.len() > u16::MAX as usize {
            return false;
        }
        out.push(id as u8);
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
        true
    }
}

impl Drop for Elements {
    fn drop(&mut self) {
        for data in self.slots.iter_mut().flatten() {
            wipe(data);
        }
    }
}

impl fmt::Debug for Elements {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let valid: Vec<(usize, usize)> = (1..MAX_ELEMENTS)
            .filter_map(|id| self.get(id).map(|data| (id, data.len())))
            .collect();
        f.debug_struct("Elements").field("valid", &valid).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_checks_fixed_sizes() {
        let mut elements = Elements::new();
        assert!(elements.set(5, &[0u8; 8]));
        assert!(!elements.set(5, &[0u8; 7]));
        let stored = [0u8; 8];
        assert_eq!(elements.get(5), Some(stored.as_slice()));
    }

    #[test]
    fn invalid_ids_are_rejected() {
        let mut elements = Elements::new();
        assert!(!elements.set(0, &[1]));
        assert!(!elements.set(MAX_ELEMENTS, &[1]));
        assert_eq!(elements.get(0), None);
        assert_eq!(elements.get(MAX_ELEMENTS), None);
        assert!(!elements.valid(0));
    }

    #[test]
    fn variable_sizes_and_invalidate() {
        let mut elements = Elements::new();
        assert!(elements.set(1, b"abc"));
        assert!(elements.set(2, &[]));
        assert!(elements.valid(2));
        assert_eq!(elements.get(1), Some(b"abc".as_slice()));
        elements.invalidate(1);
        assert!(!elements.valid(1));
        assert_eq!(elements.get(1), None);
        elements.init();
        assert!(!elements.valid(2));
        assert!(elements.set(26, &[0u8; 2]), "id 26 has size 2");
    }

    #[test]
    fn get_req_layout() {
        let mut elements = Elements::new();
        assert!(elements.set(13, &[0xaa; 256]));
        let mut out = Vec::new();
        assert!(elements.get_req(13, &mut out));
        assert_eq!(out.len(), 3 + 256);
        assert_eq!(&out[..3], &[13, 0x01, 0x00]);
        assert!(out[3..].iter().all(|&b| b == 0xaa));

        let mut missing = Vec::new();
        assert!(!elements.get_req(14, &mut missing));
        assert!(missing.is_empty());
    }

    #[test]
    fn debug_lists_only_valid_elements() {
        let mut elements = Elements::new();
        elements.set(5, &[0u8; 8]);
        elements.set(13, &[0u8; 256]);
        let shown = format!("{elements:?}");
        assert!(shown.contains("[(5, 8), (13, 256)]"));
    }
}
