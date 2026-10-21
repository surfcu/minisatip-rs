//! CA drivers: EN50221, DDCI, DVBAPI, hardware descrambler and CSA/AES.

pub mod en50221;

/// Placeholder scaffolding until the M5 implementation lands.
pub fn placeholder() {}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_linked() {
        super::placeholder();
    }
}
