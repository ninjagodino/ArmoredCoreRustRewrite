//! FSB4 sound bank (`sound/*.fsb`, magic `FSB4`), little-endian FMOD Ex banks.
//!
//! Header (48 bytes): `magic, u32 sample count, u32 sample-header bytes, u32 data bytes,
//! u32 version (0x00040000), u32 mode, 24-byte hash`. Each sample header begins with its
//! own `u16` size. The basic 80-byte header is `name[30]`, sample count, compressed bytes,
//! loop start, loop end, mode, frequency, volume, pan, priority, channels. Bytes past 80
//! are extra chunks and are skipped (the 360 headers are 112 bytes). Sample payloads are
//! packed in header order at `48 + sample-header bytes`.
//!
//! Mode bit `0x01000000` is XMA (FMOD `FSOUND_XMA`; samples are `0x01002020`): XMA2 packets
//! for [`crate::xma`].
//! Neither, with `0x10` (`FSOUND_16BITS`), is raw 16-bit PCM, big-endian when the bank mode has
//! `0x08` (FMOD `FSB_SOURCE_BIGENDIANPCM`): the 219 mode-`0x2130` samples of the 360
//! `com_05` / `com_06` / `com_07` / `com_11` banks (bank mode `0x48`), padded to 32 bytes.
//! A loop end of `length - 1` covers the whole sample.

use anyhow::{bail, ensure, Result};

pub const MAGIC: &[u8] = b"FSB4";
pub const HEADER: usize = 48;
pub const BASIC_SAMPLE: usize = 80;
/// FMOD `FSOUND_XMA`.
pub const MODE_XMA: u32 = 0x0100_0000;
/// FMOD `FSOUND_16BITS`.
pub const MODE_16BITS: u32 = 0x10;
/// Bank header mode: FMOD `FSB_SOURCE_BIGENDIANPCM`.
pub const BANK_BIG_ENDIAN_PCM: u32 = 0x08;
const VERSION: u32 = 0x0004_0000;

#[derive(Debug, Clone)]
pub struct Bank {
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub name: String,
    /// Uncompressed sample count.
    pub length: u32,
    pub loop_start: u32,
    pub loop_end: u32,
    pub mode: u32,
    pub frequency: u32,
    pub channels: u16,
    /// Bank mode [`BANK_BIG_ENDIAN_PCM`]: byte order of a [`Sample::pcm16`] payload.
    pub big_endian_pcm: bool,
    /// Payload: XMA2 packets when [`Sample::xma`], samples when [`Sample::pcm16`].
    pub data: Vec<u8>,
}

impl Sample {
    pub fn xma(&self) -> bool {
        self.mode & MODE_XMA != 0
    }

    pub fn pcm16(&self) -> bool {
        !self.xma() && self.mode & MODE_16BITS != 0
    }

    /// Decodes an XMA or 16-bit PCM sample to interleaved PCM of [`Sample::length`] frames.
    pub fn decode(&self) -> Result<crate::xma::Pcm> {
        if self.xma() {
            return crate::xma::decode(&self.data, self.channels, self.frequency, Some(self.length as usize));
        }
        ensure!(self.pcm16(), "{} is neither XMA nor 16-bit PCM (mode {:#x})", self.name, self.mode);
        let wanted = self.length as usize * self.channels as usize;
        ensure!(self.data.len() >= wanted * 2, "{}: {} PCM bytes for {wanted} samples", self.name, self.data.len());
        let samples = self.data[..wanted * 2]
            .chunks_exact(2)
            .map(|b| f32::from(if self.big_endian_pcm { i16::from_be_bytes([b[0], b[1]]) } else { i16::from_le_bytes([b[0], b[1]]) }) / 32768.0)
            .collect();
        Ok(crate::xma::Pcm { channels: self.channels, samples, errors: 0 })
    }
}

pub fn is_fsb(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

pub fn read(data: &[u8]) -> Result<Bank> {
    ensure!(is_fsb(data), "not an FSB4 bank");
    let count = lu32(data, 4)?;
    let header_bytes = lu32(data, 8)? as usize;
    let data_bytes = lu32(data, 12)? as usize;
    let version = lu32(data, 16)?;
    ensure!(version == VERSION, "FSB4 version {version:#x}, want {VERSION:#x}");
    let big_endian_pcm = lu32(data, 20)? & BANK_BIG_ENDIAN_PCM != 0;
    let headers = HEADER + header_bytes;
    let data_at = headers;
    ensure!(
        data_at.checked_add(data_bytes).is_some_and(|end| end <= data.len()),
        "FSB4 data {data_bytes:#x} at {data_at:#x} runs past {:#x}",
        data.len()
    );

    let mut at = HEADER;
    let mut samples = Vec::with_capacity(count as usize);
    let mut cursor = data_at;
    for i in 0..count {
        ensure!(at + 2 <= headers, "sample {i} header runs past the header block");
        let size = u16::from_le_bytes(data[at..at + 2].try_into().unwrap()) as usize;
        ensure!(size >= BASIC_SAMPLE, "sample {i} header is {size} bytes, want at least {BASIC_SAMPLE}");
        ensure!(at + size <= headers, "sample {i} header ({size:#x} at {at:#x}) runs past the header block");
        let name = cstr(&data[at + 2..at + 32]);
        let length = u32::from_le_bytes(data[at + 32..at + 36].try_into().unwrap());
        let compressed = u32::from_le_bytes(data[at + 36..at + 40].try_into().unwrap()) as usize;
        let loop_start = u32::from_le_bytes(data[at + 40..at + 44].try_into().unwrap());
        let loop_end = u32::from_le_bytes(data[at + 44..at + 48].try_into().unwrap());
        let mode = u32::from_le_bytes(data[at + 48..at + 52].try_into().unwrap());
        let frequency = u32::from_le_bytes(data[at + 52..at + 56].try_into().unwrap());
        let channels = u16::from_le_bytes(data[at + 62..at + 64].try_into().unwrap());
        let end = cursor.checked_add(compressed).filter(|e| *e <= data_at + data_bytes);
        let Some(end) = end else {
            bail!("sample {i} ({name}) payload {compressed:#x} at {cursor:#x} runs past the data block");
        };
        samples.push(Sample {
            name,
            length,
            loop_start,
            loop_end,
            mode,
            frequency,
            channels,
            big_endian_pcm,
            data: data[cursor..end].to_vec(),
        });
        cursor = end;
        at += size;
    }
    ensure!(cursor == data_at + data_bytes, "sample payloads summed to {:#x}, header says {data_bytes:#x}", cursor - data_at);

    Ok(Bank { samples })
}

fn lu32(data: &[u8], at: usize) -> Result<u32> {
    let Some(b) = data.get(at..at + 4) else {
        bail!("u32 at {at:#x} past end of {:#x}", data.len());
    };
    Ok(u32::from_le_bytes(b.try_into().unwrap()))
}

fn cstr(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x360_booster_bank_is_xma() {
        let Some(disc) = crate::vfs::test_disc() else { return };
        let bank = read(&disc.read("sound/se_booster.fsb").unwrap()).unwrap();
        assert_eq!(bank.samples.len(), 8);
        assert_eq!(bank.samples[4].name, "main_boost11.wav");
        let s = &bank.samples[0];
        assert_eq!((s.name.as_str(), s.mode, s.frequency, s.channels, s.length), ("boost11.wav", 0x0100_2020, 44100, 1, 81920));
        let pcm = s.decode().unwrap();
        assert_eq!((pcm.errors, pcm.samples.len()), (0, 81920));
    }

    #[test]
    fn x360_comms_bank_is_big_endian_pcm() {
        let Some(disc) = crate::vfs::test_disc() else { return };
        let bank = read(&disc.read("sound/com_07.fsb").unwrap()).unwrap();
        let s = &bank.samples[0];
        assert_eq!((s.name.as_str(), s.mode, s.frequency, s.length, s.big_endian_pcm), ("0010001.wav", 0x2130, 48000, 186308, true));
        assert!(s.pcm16());
        let pcm = s.decode().unwrap();
        assert_eq!(pcm.samples.len(), 186308);
        // Big-endian reads as a smooth waveform: neighbouring samples are close.
        let rough: f32 = pcm.samples.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / pcm.samples.iter().map(|v| v.abs()).sum::<f32>();
        assert!(rough < 0.5, "roughness {rough}");
    }

    #[test]
    fn weapon_bank_counts() {
        let Some(disc) = crate::vfs::test_disc() else { return };
        let bank = read(&disc.read("sound/se_weapon.fsb").unwrap()).unwrap();
        assert_eq!(bank.samples.len(), 124);
        assert!(bank.samples.iter().any(|s| s.name.ends_with(".wav")));
    }
}
