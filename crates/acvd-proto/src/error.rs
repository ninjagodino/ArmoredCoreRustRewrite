//! Wire-codec errors.

use std::fmt;

/// A malformed or out-of-range wire value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtoError {
    /// The reader needed more bits than the message holds.
    Truncated { needed: usize, remaining: usize },
    /// A decoded message exceeded its protocol size cap.
    TooLarge {
        name: &'static str,
        size: usize,
        max: usize,
    },
    /// A field held a value outside its documented range.
    Invalid(&'static str),
    /// A player id outside `1..=254`.
    InvalidNetId(u8),
    /// The peer speaks a different protocol revision.
    ProtocolMismatch { ours: u16, theirs: u16 },
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, remaining } => {
                write!(
                    f,
                    "truncated message: needed {needed} bits, {remaining} left"
                )
            }
            Self::TooLarge { name, size, max } => {
                write!(f, "{name} is {size} bytes, over the {max}-byte cap")
            }
            Self::Invalid(what) => write!(f, "invalid {what}"),
            Self::InvalidNetId(id) => write!(f, "invalid net id {id} (valid 1..=254)"),
            Self::ProtocolMismatch { ours, theirs } => {
                write!(f, "protocol mismatch: ours {ours}, peer {theirs}")
            }
        }
    }
}

impl std::error::Error for ProtoError {}
