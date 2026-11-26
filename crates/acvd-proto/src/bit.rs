//! Minimal LSB-first bit writer/reader.
//!
//! The wire is a sequence of little-endian bit fields: the first field occupies the low bits of
//! the first byte. Every message ends on a byte boundary with zero padding. The codec is
//! hand-rolled so the hot path has no serde/JSON and the golden-size tests below can pin the
//! exact byte layout.

use bytes::Bytes;

use crate::error::ProtoError;

/// Appends bit fields to a byte buffer.
#[derive(Clone, Debug, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    bit_len: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(bytes),
            bit_len: 0,
        }
    }

    /// Bits written so far.
    pub fn bit_len(&self) -> usize {
        self.bit_len
    }

    /// Writes the low `bits` bits of `value` (`bits <= 64`), least significant bit first.
    pub fn write_bits(&mut self, value: u64, bits: u32) {
        debug_assert!(bits <= 64);
        for i in 0..bits {
            let bit = ((value >> i) & 1) as u8;
            let byte = self.bit_len / 8;
            let offset = self.bit_len % 8;
            if byte == self.bytes.len() {
                self.bytes.push(0);
            }
            self.bytes[byte] |= bit << offset;
            self.bit_len += 1;
        }
    }

    pub fn write_bool(&mut self, v: bool) {
        self.write_bits(u64::from(v), 1);
    }

    pub fn write_u8(&mut self, v: u8) {
        self.write_bits(u64::from(v), 8);
    }

    pub fn write_i8(&mut self, v: i8) {
        self.write_bits((v as u8).into(), 8);
    }

    pub fn write_u16(&mut self, v: u16) {
        self.write_bits(u64::from(v), 16);
    }

    pub fn write_i16(&mut self, v: i16) {
        self.write_bits((v as u16).into(), 16);
    }

    pub fn write_u32(&mut self, v: u32) {
        self.write_bits(u64::from(v), 32);
    }

    pub fn write_f32(&mut self, v: f32) {
        self.write_u32(v.to_bits());
    }

    /// Pads with zero bits to the next byte boundary and returns the message.
    pub fn finish(mut self) -> Bytes {
        self.bytes.truncate(self.bit_len.div_ceil(8));
        self.bytes.into()
    }
}

/// Reads LSB-first bit fields from a byte slice.
#[derive(Clone, Debug)]
pub struct BitReader<'a> {
    bytes: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit_pos: 0 }
    }

    /// Bits not yet consumed.
    pub fn remaining_bits(&self) -> usize {
        self.bytes
            .len()
            .saturating_mul(8)
            .saturating_sub(self.bit_pos)
    }

    pub fn is_byte_aligned(&self) -> bool {
        self.bit_pos % 8 == 0
    }

    pub fn read_bits(&mut self, bits: u32) -> Result<u64, ProtoError> {
        debug_assert!(bits <= 64);
        let needed = bits as usize;
        if self.remaining_bits() < needed {
            return Err(ProtoError::Truncated {
                needed,
                remaining: self.remaining_bits(),
            });
        }
        let mut value = 0u64;
        for i in 0..bits {
            let byte = self.bytes[self.bit_pos / 8];
            let offset = self.bit_pos % 8;
            let bit = (byte >> offset) & 1;
            value |= u64::from(bit) << i;
            self.bit_pos += 1;
        }
        Ok(value)
    }

    pub fn read_bool(&mut self) -> Result<bool, ProtoError> {
        Ok(self.read_bits(1)? != 0)
    }

    pub fn read_u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.read_bits(8)? as u8)
    }

    pub fn read_i8(&mut self) -> Result<i8, ProtoError> {
        Ok(self.read_bits(8)? as u8 as i8)
    }

    pub fn read_u16(&mut self) -> Result<u16, ProtoError> {
        Ok(self.read_bits(16)? as u16)
    }

    pub fn read_i16(&mut self) -> Result<i16, ProtoError> {
        Ok(self.read_bits(16)? as u16 as i16)
    }

    pub fn read_u32(&mut self) -> Result<u32, ProtoError> {
        Ok(self.read_bits(32)? as u32)
    }

    pub fn read_f32(&mut self) -> Result<f32, ProtoError> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    /// Rejects trailing whole bytes and non-zero padding bits, so every message has exactly one
    /// encoding.
    pub fn finish(&mut self) -> Result<(), ProtoError> {
        if self.remaining_bits() >= 8 {
            return Err(ProtoError::Invalid("trailing bytes"));
        }
        while self.remaining_bits() > 0 {
            if self.read_bits(1)? != 0 {
                return Err(ProtoError::Invalid("non-zero padding"));
            }
        }
        Ok(())
    }
}
