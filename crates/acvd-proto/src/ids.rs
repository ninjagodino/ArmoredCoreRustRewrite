//! Wire identity types.

use crate::error::ProtoError;

/// Per-match player/AC identifier. `0` means "none"; `255` is reserved; `1..=254` is valid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetId(u8);

impl NetId {
    pub const NONE: Self = Self(0);
    pub const RESERVED: Self = Self(255);
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 254;

    /// A player id that can be assigned to an AC.
    pub fn new(id: u8) -> Result<Self, ProtoError> {
        if (Self::MIN..=Self::MAX).contains(&id) {
            Ok(Self(id))
        } else {
            Err(ProtoError::InvalidNetId(id))
        }
    }

    /// Accepts any non-reserved byte, including `0` for optional fields such as `lock_target`.
    pub fn from_wire(id: u8) -> Result<Self, ProtoError> {
        if id == Self::RESERVED.0 {
            Err(ProtoError::InvalidNetId(id))
        } else {
            Ok(Self(id))
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }

    pub fn is_none(self) -> bool {
        self.0 == 0
    }

    pub fn is_some(self) -> bool {
        self.0 != 0
    }
}

impl std::fmt::Display for NetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<u8> for NetId {
    type Error = ProtoError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<NetId> for u8 {
    fn from(value: NetId) -> Self {
        value.0
    }
}
