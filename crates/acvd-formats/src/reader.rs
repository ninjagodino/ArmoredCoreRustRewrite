use anyhow::{bail, Result};

/// Bounds-checked big-endian view over a byte slice.
#[derive(Clone, Copy)]
pub struct Be<'a>(pub &'a [u8]);

impl<'a> Be<'a> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn bytes(&self, at: usize, len: usize) -> Result<&'a [u8]> {
        match at.checked_add(len).and_then(|end| self.0.get(at..end)) {
            Some(b) => Ok(b),
            None => bail!("read of {len} bytes at {at:#x} runs past end of {:#x}-byte buffer", self.0.len()),
        }
    }

    fn array<const N: usize>(&self, at: usize) -> Result<[u8; N]> {
        Ok(self.bytes(at, N)?.try_into().expect("length checked"))
    }

    pub fn u8(&self, at: usize) -> Result<u8> {
        Ok(self.array::<1>(at)?[0])
    }
    pub fn i8(&self, at: usize) -> Result<i8> {
        Ok(self.u8(at)? as i8)
    }
    pub fn u16(&self, at: usize) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array(at)?))
    }
    pub fn i16(&self, at: usize) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array(at)?))
    }
    pub fn u32(&self, at: usize) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array(at)?))
    }
    pub fn i32(&self, at: usize) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array(at)?))
    }
    pub fn f32(&self, at: usize) -> Result<f32> {
        Ok(f32::from_be_bytes(self.array(at)?))
    }

    /// Fixed-width ASCII field: text up to the first NUL, trailing space padding removed.
    pub fn fixstr(&self, at: usize, len: usize) -> Result<String> {
        Ok(String::from_utf8_lossy(until_nul(self.bytes(at, len)?)).trim_end().to_owned())
    }

    /// Fixed-width Shift-JIS field, terminated by the first NUL.
    pub fn fixstr_sjis(&self, at: usize, len: usize) -> Result<String> {
        Ok(sjis(until_nul(self.bytes(at, len)?)))
    }

    /// NUL-terminated Shift-JIS string starting at `at`.
    pub fn cstr_sjis(&self, at: usize) -> Result<String> {
        let Some(tail) = self.0.get(at..) else {
            bail!("string offset {at:#x} past end of {:#x}-byte buffer", self.0.len());
        };
        Ok(sjis(until_nul(tail)))
    }
}

pub fn until_nul(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

pub fn sjis(b: &[u8]) -> String {
    encoding_rs::SHIFT_JIS.decode_without_bom_handling(b).0.into_owned()
}

/// UTF-16BE text up to the first NUL code unit.
pub fn utf16be(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}
