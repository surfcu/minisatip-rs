//! CI+ content control building blocks: crypto, credential elements,
//! certificates and authdata (`ca.cpp`). The protocol handler is separate.

pub mod authdata;
pub mod cc;
pub mod certs;
pub mod crypto;
pub mod elements;
mod ffi;
#[cfg(test)]
pub(crate) mod test_pki;

/// `MAX_ELEMENTS` (ca.h): element table size, id 55 is the last usable.
pub const MAX_ELEMENTS: usize = 56;
/// `MAX_PAIRS` (ca.h): stored authentication records read at once.
pub const MAX_PAIRS: usize = 10;
