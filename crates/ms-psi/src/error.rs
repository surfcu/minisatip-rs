//! Errors returned by PSI section parsers.

use core::fmt;

/// Reason a PSI section could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsiError {
    /// The section is shorter than declared or an internal length overruns it.
    Truncated,
    /// The section carries a different `table_id` than the parser expects.
    BadTableId { expected: u8, found: u8 },
    /// The trailing CRC-32 does not match the section contents.
    CrcMismatch,
    /// A descriptor length does not fit its own payload.
    BadDescriptor,
}

impl fmt::Display for PsiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("truncated PSI section"),
            Self::BadTableId { expected, found } => {
                write!(
                    formatter,
                    "expected table_id {expected:#04X}, found {found:#04X}"
                )
            }
            Self::CrcMismatch => formatter.write_str("PSI section CRC mismatch"),
            Self::BadDescriptor => formatter.write_str("malformed descriptor"),
        }
    }
}

impl std::error::Error for PsiError {}
