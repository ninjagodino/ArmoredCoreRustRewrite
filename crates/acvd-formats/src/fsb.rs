//! FSB4 sound bank (`sound/*.fsb`, magic `FSB4`). Little-endian, the FMOD Ex bank the PS3
//! build plays. The Xbox 360 build uses the same event names against XMA banks; this reader
//! is the PS3 MPEG banks the rewrite already loads from the disc.
//!
//! Header (48 bytes): `magic, u32 sample count, u32 sample-header bytes, u32 data bytes,
//! u32 version (0x00040000), u32 mode, 24-byte hash`. Each sample header begins with its
//! own `u16` size. The basic 80-byte header is `name[30]`, sample count, compressed bytes,
//! loop start, loop end, mode, frequency, volume, pan, priority, channels. Bytes past 80
//! are extra chunks and are skipped. Sample payloads are packed in header order at
//! `48 + sample-header bytes`.
//!
//! Mode bit `0x200` is MPEG (FMOD `FSOUND_MPEG`): `se_booster.fsb` payloads start `FF FB`.
//! Each MPEG frame is padded out to a multiple of 4 bytes.
//! A loop end of `length - 1` covers the whole sample.

use anyhow::{bail, ensure, Result};

pub const MAGIC: &[u8] = b"FSB4";
pub const HEADER: usize = 48;
pub const BASIC_SAMPLE: usize = 80;
/// FMOD `FSOUND_MPEG`.
pub const MODE_MPEG: u32 = 0x200;
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
    /// Compressed payload (MPEG frames when [`Sample::mpeg`]).
    pub data: Vec<u8>,
}

impl Sample {
    pub fn mpeg(&self) -> bool {
        self.mode & MODE_MPEG != 0
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

    fn disc(name: &str) -> Option<Vec<u8>> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join("ACVD Unbound").join("PS3_GAME/USRDIR/sound").join(name);
        std::fs::read(path).ok()
    }

    #[test]
    fn booster_bank() {
        let Some(data) = disc("se_booster.fsb") else { return };
        let bank = read(&data).unwrap();
        assert_eq!(bank.samples.len(), 8);
        assert_eq!(bank.samples[0].name, "boost11.wav");
        assert_eq!(bank.samples[4].name, "main_boost11.wav");
        assert!(bank.samples.iter().all(|s| s.mpeg() && s.channels == 1 && s.frequency == 44100));
        assert_eq!(bank.samples[0].loop_end, bank.samples[0].length - 1);
        assert_eq!(&bank.samples[0].data[..2], &[0xFF, 0xFB]);
    }

    #[test]
    fn weapon_bank_counts() {
        let Some(data) = disc("se_weapon.fsb") else { return };
        let bank = read(&data).unwrap();
        assert_eq!(bank.samples.len(), 124);
        assert!(bank.samples.iter().any(|s| s.name.ends_with(".wav")));
    }
}
