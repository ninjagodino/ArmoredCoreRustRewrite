//! XMA2 decoder for the Xbox 360 FSB4 banks ([`crate::fsb`] samples with mode
//! [`crate::fsb::MODE_XMA`]).
//!
//! This module is a port of FFmpeg's `libavcodec/wmaprodec.c` (XMA2 path) and the run-level
//! decoder of `wma.c`, with the tables of `wmaprodata.h`. It is licensed under the GNU Lesser
//! General Public License, version 2.1 or later, like the code it is derived from:
//! Copyright (c) 2007 Baptiste Coudurier, Benjamin Larsson, Ulion; (c) 2008 - 2011 Sascha
//! Sommer, Benjamin Larsson.
//!
//! **Stream**: 2048-byte packets. Each starts with a big-endian `u32`: frame count (6 bits),
//! bits of the frame carried over from the previous packet (15), metadata (3), packets to
//! skip before this stream's next one (8). Frames are a bitstream that runs across packets:
//! a 15-bit length (in bits, counting the length field), the WMA Pro frame, a trailer bit set
//! when another frame follows in the packet. Frames that do not fit are padded with 1 bits.
//! A sample of N channels is `ceil(N / 2)` streams of two channels (the last one mono when N
//! is odd) whose packets interleave by the skip counts.
//!
//! **Frames**: WMA Pro with the fixed XMA decode flags `0x10d6` (length prefix, dynamic
//! range byte, up to 4 subframes), 512 samples per frame, 16-bit scale. The first frame only
//! primes the overlap and the decoder drops 64 more samples, as FFmpeg does; the end flushes
//! half a frame. Callers trim to the bank's sample count.
//!
//! Only 1- and 2-channel streams exist in XMA, so the WMA Pro paths for groups of more than
//! two channels and the LFE channel are left out. The IMDCT is computed directly from a
//! cosine table per block size (blocks are at most 512 coefficients).

use std::sync::OnceLock;

use anyhow::{bail, ensure, Result};

pub const PACKET: usize = 2048;
/// Samples per frame and channel.
const FRAME: usize = 512;
/// XMA decode flags `0x10d6`: subframe count `1 << ((0xd6 & 0x38) >> 3)`.
const MAX_SUBFRAMES_FRAME: usize = 4;
const MIN_SUBFRAME: usize = FRAME / MAX_SUBFRAMES_FRAME;
/// `av_log2(log2 max subframes) + 1`.
const SUBFRAME_LEN_BITS: u32 = 2;
/// `av_log2(2048) + 4`: width of the frame-length prefix and of the carried-over bit count.
const LOG2_FRAME_SIZE: u32 = 15;
/// Block sizes 512, 256, 128.
const BLOCK_SIZES: usize = 3;
const MAX_SUBFRAMES: usize = 32;
const MAX_BANDS: usize = 29;
/// `90 * bits_per_sample >> 4` with 16 bits per sample.
const BASE_QUANT: i32 = 90;
/// Samples FFmpeg drops from the start of an XMA stream (after the priming frame).
const START_SKIP: usize = 64;

/// Decoded PCM, interleaved.
#[derive(Debug, Clone)]
pub struct Pcm {
    pub channels: u16,
    pub samples: Vec<f32>,
    /// Frames the decoder rejected (each drops the rest of its packet).
    pub errors: usize,
}

impl Pcm {
    pub fn to_i16(&self) -> Vec<i16> {
        self.samples
            .iter()
            .map(|s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect()
    }
}

/// Decodes an XMA2 payload of `channels` channels. `length` (samples per channel, from the
/// bank header) trims or pads the result.
pub fn decode(data: &[u8], channels: u16, sample_rate: u32, length: Option<usize>) -> Result<Pcm> {
    ensure!(channels > 0, "XMA with no channels");
    let streams_n = (channels as usize).div_ceil(2);
    let mut streams: Vec<Stream> = (0..streams_n)
        .map(|i| {
            Stream::new(
                if i + 1 == streams_n && channels % 2 == 1 {
                    1
                } else {
                    2
                },
                sample_rate,
            )
        })
        .collect();
    let mut current = 0;
    for packet in data.chunks_exact(PACKET) {
        streams[current].packet(packet);
        if streams[current].skip_packets != 0 {
            let mut min = (streams[0].skip_packets, 0);
            for (i, s) in streams.iter().enumerate().skip(1) {
                if s.skip_packets < min.0 {
                    min = (s.skip_packets, i);
                }
            }
            current = min.1;
        }
        for s in &mut streams {
            s.skip_packets = s.skip_packets.saturating_sub(1);
        }
    }
    for s in &mut streams {
        s.flush();
    }

    let frames = streams
        .iter()
        .map(|s| s.pcm[0].len())
        .min()
        .unwrap_or(0)
        .saturating_sub(START_SKIP);
    let frames_out = length.unwrap_or(frames);
    let mut samples = Vec::with_capacity(frames_out * channels as usize);
    for i in 0..frames_out {
        for s in &streams {
            for ch in &s.pcm {
                samples.push(if i < frames { ch[START_SKIP + i] } else { 0.0 });
            }
        }
    }
    Ok(Pcm {
        channels,
        samples,
        errors: streams.iter().map(|s| s.errors).sum(),
    })
}

/// MSB-first bit reader; reads past the end return zeros.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    len: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], len: usize) -> Self {
        Self { data, pos: 0, len }
    }

    fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos / 8;
        let mut v = 0u64;
        for i in 0..5 {
            v = (v << 8) | *self.data.get(byte + i).unwrap_or(&0) as u64;
        }
        let shift = 40 - (self.pos % 8) as u32 - n;
        ((v >> shift) & ((1u64 << n) - 1)) as u32
    }

    fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    fn bit(&mut self) -> bool {
        self.read(1) == 1
    }

    fn signed(&mut self, n: u32) -> i32 {
        let v = self.read(n) as i32;
        (v << (32 - n)) >> (32 - n)
    }

    fn skip(&mut self, n: usize) {
        self.pos += n;
    }

    fn left(&self) -> isize {
        self.len as isize - self.pos as isize
    }
}

/// Growable bit buffer for a frame that spans packets.
#[derive(Default)]
struct BitBuf {
    data: Vec<u8>,
    len: usize,
}

impl BitBuf {
    fn push(&mut self, src: &mut Bits, n: usize) {
        for _ in 0..n {
            let bit = src.bit();
            if self.len % 8 == 0 {
                self.data.push(0);
            }
            if bit {
                *self.data.last_mut().unwrap() |= 0x80 >> (self.len % 8);
            }
            self.len += 1;
        }
    }

    /// The frame length prefix, once enough bits are in.
    fn frame_len(&self) -> Option<usize> {
        (self.len >= LOG2_FRAME_SIZE as usize)
            .then(|| Bits::new(&self.data, self.len).peek(LOG2_FRAME_SIZE) as usize)
    }
}

/// Canonical Huffman table from code lengths in symbol-list order (FFmpeg
/// `ff_vlc_init_from_lengths`): codes count up from 0, left-aligned in 32 bits.
struct Vlc {
    codes: Vec<(u32, u8, i32)>,
}

impl Vlc {
    fn new(entries: impl IntoIterator<Item = (u8, i32)>, offset: i32) -> Self {
        let mut codes = Vec::new();
        let mut code = 0u64;
        for (len, sym) in entries {
            codes.push((code as u32, len, sym + offset));
            code += 1u64 << (32 - len);
        }
        Self { codes }
    }

    /// The symbol, or -1 for a code not in the table (nothing consumed).
    fn read(&self, gb: &mut Bits) -> i32 {
        let v = gb.peek(32);
        let i = self.codes.partition_point(|c| c.0 <= v);
        if i == 0 {
            return -1;
        }
        let (code, len, sym) = self.codes[i - 1];
        if (v ^ code) >> (32 - len as u32) != 0 {
            return -1;
        }
        gb.skip(len as usize);
        sym
    }
}

struct Tables {
    sf: Vlc,
    sf_rl: Vlc,
    coef: [Vlc; 2],
    vec4: Vlc,
    vec2: Vlc,
    vec1: Vlc,
    /// Sine windows of 128, 256, 512 samples.
    windows: [Vec<f32>; BLOCK_SIZES],
    /// IMDCT cosine tables (scale included) for 128, 256, 512 coefficients.
    imdct: [Vec<f32>; BLOCK_SIZES],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let pairs = |t: &[[u16; 2]]| {
            t.iter()
                .map(|e| (e[1] as u8, e[0] as i32))
                .collect::<Vec<_>>()
        };
        let block = |i: usize| MIN_SUBFRAME << i;
        Tables {
            sf: Vlc::new(pairs(&SCALE_TABLE), -60),
            sf_rl: Vlc::new(pairs(&SCALE_RL_TABLE), 0),
            coef: [
                Vlc::new(
                    COEF0_LENS
                        .into_iter()
                        .zip(COEF0_SYMS)
                        .map(|(l, s)| (l, s as i32)),
                    0,
                ),
                Vlc::new(pairs(&COEF1_TABLE), 0),
            ],
            vec4: Vlc::new(
                VEC4_LENS
                    .into_iter()
                    .zip(VEC4_SYMS)
                    .map(|(l, s)| (l, s as i32)),
                -1,
            ),
            vec2: Vlc::new(pairs(&VEC2_TABLE), -1),
            vec1: Vlc::new(pairs(&VEC1_TABLE), 0),
            windows: std::array::from_fn(|i| {
                let n = block(i);
                (0..n)
                    .map(|k| {
                        ((k as f64 + 0.5) * std::f64::consts::PI / (2.0 * n as f64)).sin() as f32
                    })
                    .collect()
            }),
            imdct: std::array::from_fn(|i| {
                // FFmpeg's half-length inverse MDCT (`av_tx` MDCT, scale 1 / (N/2) / 2^15):
                // out[n] = -scale * sum_k X[k] cos(pi / (4N) * (2n + 2N + 1) * (2k + 1)).
                let n = block(i);
                let scale = -1.0 / (n as f64 / 2.0) / 32768.0;
                let period = 8 * n;
                let mut t = vec![0f32; n * n];
                for j in 0..n {
                    for k in 0..n {
                        let phase = ((2 * j + 2 * n + 1) * (2 * k + 1)) % period;
                        t[j * n + k] = (scale
                            * (std::f64::consts::PI * phase as f64 / (4.0 * n as f64)).cos())
                            as f32;
                    }
                }
                t
            }),
        }
    })
}

fn log2(v: usize) -> usize {
    (usize::BITS - 1 - v.max(1).leading_zeros()) as usize
}

/// FFmpeg `ff_wma_get_large_val`.
fn large_val(gb: &mut Bits) -> u32 {
    let mut n = 8;
    if gb.bit() {
        n += 8;
        if gb.bit() {
            n += 8;
            if gb.bit() {
                n += 7;
            }
        }
    }
    gb.read(n)
}

#[derive(Clone)]
struct Channel {
    prev_block_len: usize,
    transmit_coefs: bool,
    num_subframes: usize,
    subframe_len: [usize; MAX_SUBFRAMES],
    cur_subframe: usize,
    decoded_samples: usize,
    grouped: bool,
    quant_step: i32,
    reuse_sf: bool,
    scale_factor_step: i32,
    max_scale_factor: i32,
    saved_scale_factors: [[i32; MAX_BANDS]; 2],
    scale_factor_idx: usize,
    /// Which `saved_scale_factors` buffer the current subframe uses.
    sf_buf: usize,
    table_idx: usize,
    /// Start of the current subframe's coefficients in `out`.
    coeffs: usize,
    num_vec_coeffs: usize,
    out: Vec<f32>,
}

struct Group {
    transform: bool,
    transform_band: [bool; MAX_BANDS],
    matrix: [f32; 4],
    channels: Vec<usize>,
}

/// One XMA stream: a WMA Pro decoder of 1 or 2 channels.
struct Stream {
    nb_channels: usize,
    num_sfb: [usize; BLOCK_SIZES],
    sfb_offsets: [[usize; MAX_BANDS]; BLOCK_SIZES],
    sf_offsets: [[[usize; MAX_BANDS]; BLOCK_SIZES]; BLOCK_SIZES],
    channel: Vec<Channel>,
    skip_frame: bool,
    parsed_all_subframes: bool,
    subframe_len: usize,
    channels_for_cur_subframe: usize,
    channel_indexes: [usize; 2],
    num_bands: usize,
    transmit_num_vec_coeffs: bool,
    table_idx: usize,
    esc_len: u32,
    groups: Vec<Group>,
    tmp: Vec<f32>,
    pending: Option<BitBuf>,
    skip_packets: u32,
    pcm: Vec<Vec<f32>>,
    errors: usize,
}

impl Stream {
    fn new(nb_channels: usize, sample_rate: u32) -> Self {
        // FFmpeg `get_rate` for XMA: band edges are laid out for the next standard rate.
        let rate = match sample_rate {
            r if r > 44100 => 48000,
            r if r > 32000 => 44100,
            r if r > 24000 => 32000,
            _ => 24000,
        };
        let mut num_sfb = [0; BLOCK_SIZES];
        let mut sfb_offsets = [[0; MAX_BANDS]; BLOCK_SIZES];
        for i in 0..BLOCK_SIZES {
            let subframe_len = FRAME >> i;
            let offs = &mut sfb_offsets[i];
            let mut band = 1;
            for &freq in CRITICAL_FREQ.iter().take(MAX_BANDS - 1) {
                if offs[band - 1] >= subframe_len {
                    break;
                }
                let offset = ((subframe_len * 2 * freq as usize) / rate + 2) & !3;
                if offset > offs[band - 1] {
                    offs[band] = offset;
                    band += 1;
                }
                if offset >= subframe_len {
                    break;
                }
            }
            offs[band - 1] = subframe_len;
            num_sfb[i] = band - 1;
        }
        let mut sf_offsets = [[[0; MAX_BANDS]; BLOCK_SIZES]; BLOCK_SIZES];
        for i in 0..BLOCK_SIZES {
            for b in 0..num_sfb[i] {
                let offset = ((sfb_offsets[i][b] + sfb_offsets[i][b + 1] - 1) << i) >> 1;
                for x in 0..BLOCK_SIZES {
                    let mut v = 0;
                    while v + 1 < MAX_BANDS && sfb_offsets[x][v + 1] << x < offset {
                        v += 1;
                    }
                    sf_offsets[i][x][b] = v;
                }
            }
        }
        let channel = Channel {
            prev_block_len: FRAME,
            transmit_coefs: false,
            num_subframes: 0,
            subframe_len: [0; MAX_SUBFRAMES],
            cur_subframe: 0,
            decoded_samples: 0,
            grouped: false,
            quant_step: 0,
            reuse_sf: false,
            scale_factor_step: 0,
            max_scale_factor: 0,
            saved_scale_factors: [[0; MAX_BANDS]; 2],
            scale_factor_idx: 0,
            sf_buf: 0,
            table_idx: 0,
            coeffs: 0,
            num_vec_coeffs: 0,
            out: vec![0.0; FRAME * 2],
        };
        Self {
            nb_channels,
            num_sfb,
            sfb_offsets,
            sf_offsets,
            channel: vec![channel; nb_channels],
            skip_frame: true,
            parsed_all_subframes: false,
            subframe_len: 0,
            channels_for_cur_subframe: 0,
            channel_indexes: [0; 2],
            num_bands: 0,
            transmit_num_vec_coeffs: false,
            table_idx: 0,
            esc_len: 0,
            groups: Vec::new(),
            tmp: vec![0.0; FRAME],
            pending: None,
            skip_packets: 0,
            pcm: vec![Vec::new(); nb_channels],
            errors: 0,
        }
    }

    fn packet(&mut self, packet: &[u8]) {
        let total = PACKET * 8;
        let mut gb = Bits::new(packet, total);
        gb.skip(6);
        let carried = gb.read(LOG2_FRAME_SIZE) as usize;
        gb.skip(3);
        self.skip_packets = gb.read(8);

        if carried == 0 {
            self.pending = None;
        } else {
            let take = carried.min(gb.left() as usize);
            match self.pending.as_mut() {
                Some(buf) => {
                    buf.push(&mut gb, take);
                    if buf.frame_len().is_some_and(|len| buf.len >= len) {
                        let buf = self.pending.take().unwrap();
                        if self.frame_bits(&buf.data, buf.len).is_err() {
                            self.errors += 1;
                        }
                    }
                }
                None => gb.skip(take),
            }
        }

        loop {
            let left = gb.left();
            if left <= 0 {
                return;
            }
            let len = gb.peek(LOG2_FRAME_SIZE) as usize;
            if left > LOG2_FRAME_SIZE as isize && len != 0 && len as isize <= left {
                let start = gb.pos;
                let mut buf = BitBuf::default();
                buf.push(&mut gb, len);
                debug_assert_eq!(gb.pos, start + len);
                match self.frame_bits(&buf.data, buf.len) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(_) => {
                        self.errors += 1;
                        self.pending = None;
                        return;
                    }
                }
            }
            let mut buf = BitBuf::default();
            let rest = gb.left().max(0) as usize;
            buf.push(&mut gb, rest);
            self.pending = Some(buf);
            return;
        }
    }

    /// Decodes one whole frame; returns its trailer bit (another frame follows).
    fn frame_bits(&mut self, data: &[u8], len_bits: usize) -> Result<bool> {
        let mut gb = Bits::new(data, len_bits);
        self.frame(&mut gb, len_bits)
    }

    fn flush(&mut self) {
        for (c, ch) in self.channel.iter().enumerate() {
            self.pcm[c].extend_from_slice(&ch.out[..FRAME / 2]);
            self.pcm[c].extend(std::iter::repeat_n(0.0, FRAME / 2));
        }
    }

    fn frame(&mut self, gb: &mut Bits, saved_bits: usize) -> Result<bool> {
        let len = gb.read(LOG2_FRAME_SIZE) as usize;
        self.tile_header(gb)?;
        if self.nb_channels > 1 && gb.bit() && gb.bit() {
            gb.skip(4 * self.nb_channels * self.nb_channels);
        }
        // Dynamic range gain (flag 0x80): read and not applied, as in FFmpeg.
        gb.skip(8);
        if gb.bit() {
            let bits = log2(FRAME * 2) as u32;
            if gb.bit() {
                gb.skip(bits as usize);
            }
            if gb.bit() {
                gb.skip(bits as usize);
            }
        }
        self.parsed_all_subframes = false;
        for ch in &mut self.channel {
            ch.decoded_samples = 0;
            ch.cur_subframe = 0;
            ch.reuse_sf = false;
        }
        while !self.parsed_all_subframes {
            self.subframe(gb, saved_bits)?;
        }
        for (c, ch) in self.channel.iter_mut().enumerate() {
            if !self.skip_frame {
                self.pcm[c].extend_from_slice(&ch.out[..FRAME]);
            }
            ch.out.copy_within(FRAME..FRAME + FRAME / 2, 0);
        }
        self.skip_frame = false;
        ensure!(
            len == gb.pos + 2,
            "frame length {len} bits, decoded {}",
            gb.pos
        );
        gb.skip(len - gb.pos - 1);
        Ok(gb.bit())
    }

    fn subframe_length(&self, gb: &mut Bits, offset: usize) -> Result<usize> {
        if offset == FRAME - MIN_SUBFRAME {
            return Ok(MIN_SUBFRAME);
        }
        ensure!(gb.left() >= 1, "subframe length past the frame");
        // Four subframes: the first bit marks a maximum-length subframe.
        let shift = if gb.bit() {
            1 + gb.read(SUBFRAME_LEN_BITS - 1)
        } else {
            0
        };
        let len = FRAME >> shift;
        ensure!(
            (MIN_SUBFRAME..=FRAME).contains(&len),
            "subframe length {len}"
        );
        Ok(len)
    }

    fn tile_header(&mut self, gb: &mut Bits) -> Result<()> {
        let n = self.nb_channels;
        let mut num_samples = [0usize; 2];
        let mut contains = [false; 2];
        let mut channels_for_cur = n;
        let mut min_channel_len = 0;
        for ch in &mut self.channel {
            ch.num_subframes = 0;
        }
        let fixed = gb.bit();
        loop {
            for c in 0..n {
                contains[c] = num_samples[c] == min_channel_len
                    && (fixed
                        || channels_for_cur == 1
                        || min_channel_len == FRAME - MIN_SUBFRAME
                        || gb.bit());
            }
            let len = self.subframe_length(gb, min_channel_len)?;
            min_channel_len += len;
            for c in 0..n {
                let ch = &mut self.channel[c];
                if contains[c] {
                    ensure!(ch.num_subframes < MAX_SUBFRAMES, "more than 31 subframes");
                    ch.subframe_len[ch.num_subframes] = len;
                    num_samples[c] += len;
                    ch.num_subframes += 1;
                    ensure!(num_samples[c] <= FRAME, "channel longer than the frame");
                } else if num_samples[c] <= min_channel_len {
                    if num_samples[c] < min_channel_len {
                        channels_for_cur = 0;
                        min_channel_len = num_samples[c];
                    }
                    channels_for_cur += 1;
                }
            }
            if min_channel_len >= FRAME {
                return Ok(());
            }
        }
    }

    fn subframe(&mut self, gb: &mut Bits, saved_bits: usize) -> Result<()> {
        let t = tables();
        let mut offset = FRAME;
        let mut subframe_len = FRAME;
        let mut total = FRAME * self.nb_channels;
        for ch in &mut self.channel {
            ch.grouped = false;
            if offset > ch.decoded_samples {
                offset = ch.decoded_samples;
                subframe_len = ch.subframe_len.get(ch.cur_subframe).copied().unwrap_or(0);
            }
        }
        self.channels_for_cur_subframe = 0;
        for c in 0..self.nb_channels {
            let ch = &mut self.channel[c];
            total -= ch.decoded_samples;
            let len = ch.subframe_len.get(ch.cur_subframe).copied().unwrap_or(0);
            if offset == ch.decoded_samples && subframe_len == len {
                total -= len;
                ch.decoded_samples += len;
                self.channel_indexes[self.channels_for_cur_subframe] = c;
                self.channels_for_cur_subframe += 1;
            }
        }
        ensure!(
            subframe_len >= MIN_SUBFRAME,
            "subframe length {subframe_len}"
        );
        if total == 0 {
            self.parsed_all_subframes = true;
        }

        self.table_idx = log2(FRAME / subframe_len);
        self.num_bands = self.num_sfb[self.table_idx];
        let coeffs = offset + FRAME / 2;
        for &c in &self.channel_indexes[..self.channels_for_cur_subframe] {
            self.channel[c].coeffs = coeffs;
        }
        self.subframe_len = subframe_len;
        self.esc_len = log2(subframe_len - 1) as u32 + 1;

        // Extended header: fill bits.
        if gb.bit() {
            let mut fill = gb.read(2) as usize;
            if fill == 0 {
                let len = gb.read(4);
                fill = gb.read(len) as usize + 1;
            }
            ensure!(gb.pos + fill <= saved_bits, "fill bits past the frame");
            gb.skip(fill);
        }
        ensure!(!gb.bit(), "reserved subframe bit");

        self.channel_transform(gb)?;

        let cur = self.channels_for_cur_subframe;
        let mut transmit = false;
        for i in 0..cur {
            let c = self.channel_indexes[i];
            self.channel[c].transmit_coefs = gb.bit();
            transmit |= self.channel[c].transmit_coefs;
        }

        if transmit {
            self.transmit_num_vec_coeffs = gb.bit();
            if self.transmit_num_vec_coeffs {
                let bits = log2(subframe_len.div_ceil(4)) as u32 + 1;
                for i in 0..cur {
                    let c = self.channel_indexes[i];
                    let n = (gb.read(bits) as usize) << 2;
                    ensure!(n <= subframe_len, "{n} vector coefficients");
                    self.channel[c].num_vec_coeffs = n;
                }
            } else {
                for i in 0..cur {
                    let c = self.channel_indexes[i];
                    self.channel[c].num_vec_coeffs = subframe_len;
                }
            }
            let mut step = gb.signed(6);
            let mut quant_step = BASE_QUANT + step;
            if step == -32 || step == 31 {
                let sign = if step == 31 { 0 } else { -1 };
                let mut quant = 0;
                while gb.pos + 5 < saved_bits {
                    step = gb.read(5) as i32;
                    if step != 31 {
                        break;
                    }
                    quant += 31;
                }
                quant_step += ((quant + step) ^ sign) - sign;
            }
            if cur == 1 {
                self.channel[self.channel_indexes[0]].quant_step = quant_step;
            } else {
                let modifier_len = gb.read(3);
                for i in 0..cur {
                    let c = self.channel_indexes[i];
                    self.channel[c].quant_step = quant_step;
                    if gb.bit() {
                        self.channel[c].quant_step += if modifier_len > 0 {
                            gb.read(modifier_len) as i32 + 1
                        } else {
                            1
                        };
                    }
                }
            }
            self.scale_factors(gb)?;
        }

        for i in 0..cur {
            let c = self.channel_indexes[i];
            if self.channel[c].transmit_coefs && gb.pos < saved_bits {
                // FFmpeg ignores a run-level overflow here; the coefficients stay as decoded.
                let _ = self.coeffs(gb, c);
            } else {
                let ch = &mut self.channel[c];
                ch.out[ch.coeffs..ch.coeffs + subframe_len].fill(0.0);
            }
        }

        if transmit {
            self.inverse_channel_transform();
            let size = log2(subframe_len) - log2(MIN_SUBFRAME);
            let cos = &t.imdct[size];
            for i in 0..cur {
                let c = self.channel_indexes[i];
                let ch = &mut self.channel[c];
                let sf = ch.saved_scale_factors[ch.sf_buf];
                let sfb = &self.sfb_offsets[self.table_idx];
                for b in 0..self.num_bands {
                    let end = sfb[b + 1].min(subframe_len);
                    let exp = ch.quant_step - (ch.max_scale_factor - sf[b]) * ch.scale_factor_step;
                    let quant = 10f64.powf(exp as f64 / 20.0) as f32;
                    for k in sfb[b]..end {
                        self.tmp[k] = ch.out[ch.coeffs + k] * quant;
                    }
                }
                let n = subframe_len;
                let src = &self.tmp[..n];
                for j in 0..n {
                    let row = &cos[j * n..j * n + n];
                    ch.out[ch.coeffs + j] = row.iter().zip(src).map(|(a, b)| a * b).sum();
                }
            }
        }

        self.window();

        for i in 0..cur {
            let ch = &mut self.channel[self.channel_indexes[i]];
            ensure!(ch.cur_subframe < ch.num_subframes, "broken subframe");
            ch.cur_subframe += 1;
        }
        Ok(())
    }

    fn channel_transform(&mut self, gb: &mut Bits) -> Result<()> {
        self.groups.clear();
        if self.nb_channels < 2 {
            return Ok(());
        }
        ensure!(!gb.bit(), "channel transform bit");
        let mut remaining = self.channels_for_cur_subframe;
        while remaining > 0 && self.groups.len() < self.channels_for_cur_subframe {
            let mut g = Group {
                transform: false,
                transform_band: [false; MAX_BANDS],
                matrix: [0.0; 4],
                channels: Vec::new(),
            };
            let num = remaining;
            for &c in &self.channel_indexes[..self.channels_for_cur_subframe] {
                if !self.channel[c].grouped {
                    g.channels.push(c);
                }
                self.channel[c].grouped = true;
            }
            if num == 2 {
                if gb.bit() {
                    ensure!(!gb.bit(), "unknown channel transform type");
                } else {
                    g.transform = true;
                    g.matrix = [1.0, -1.0, 1.0, 1.0];
                }
            }
            if g.transform {
                if gb.bit() {
                    g.transform_band[..self.num_bands].fill(true);
                } else {
                    for b in 0..self.num_bands {
                        g.transform_band[b] = gb.bit();
                    }
                }
            }
            remaining -= num;
            self.groups.push(g);
        }
        Ok(())
    }

    fn scale_factors(&mut self, gb: &mut Bits) -> Result<()> {
        let t = tables();
        for i in 0..self.channels_for_cur_subframe {
            let c = self.channel_indexes[i];
            let num_bands = self.num_bands;
            let resample = self.sf_offsets[self.table_idx][self.channel[c].table_idx];
            let ch = &mut self.channel[c];
            ch.sf_buf = 1 - ch.scale_factor_idx;
            let (cur, saved) = (ch.sf_buf, ch.scale_factor_idx);
            if ch.reuse_sf {
                for b in 0..num_bands {
                    ch.saved_scale_factors[cur][b] = ch.saved_scale_factors[saved][resample[b]];
                }
            }
            if ch.cur_subframe == 0 || gb.bit() {
                if !ch.reuse_sf {
                    ch.scale_factor_step = gb.read(2) as i32 + 1;
                    let mut val = 45 / ch.scale_factor_step;
                    for b in 0..num_bands {
                        val += t.sf.read(gb);
                        ch.saved_scale_factors[cur][b] = val;
                    }
                } else {
                    let mut b = 0;
                    while b < num_bands {
                        let idx = t.sf_rl.read(gb);
                        let (val, sign, skip) = if idx == 0 {
                            let code = gb.read(14) as i32;
                            (code >> 6, (code & 1) - 1, ((code & 0x3f) >> 1) as usize)
                        } else if idx == 1 {
                            break;
                        } else {
                            let idx = idx.max(0) as usize;
                            (
                                SCALE_RL_LEVEL[idx] as i32,
                                gb.bit() as i32 - 1,
                                SCALE_RL_RUN[idx] as usize,
                            )
                        };
                        b += skip;
                        ensure!(b < num_bands, "invalid scale factor coding");
                        ch.saved_scale_factors[cur][b] += (val ^ sign) - sign;
                        b += 1;
                    }
                }
                ch.scale_factor_idx = 1 - ch.scale_factor_idx;
                ch.table_idx = self.table_idx;
                ch.reuse_sf = true;
            }
            ch.max_scale_factor = ch.saved_scale_factors[cur][..num_bands]
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
        }
        Ok(())
    }

    fn coeffs(&mut self, gb: &mut Bits, c: usize) -> Result<()> {
        let t = tables();
        let subframe_len = self.subframe_len;
        let transmit_num_vec = self.transmit_num_vec_coeffs;
        let esc_len = self.esc_len;
        let ch = &mut self.channel[c];
        let base = ch.coeffs;
        let table = gb.bit() as usize;
        let (vlc, run, level): (&Vlc, &[u16], &[u8]) = if table == 1 {
            (&t.coef[1], &COEF1_RUN, &COEF1_LEVEL)
        } else {
            (&t.coef[0], &COEF0_RUN, &COEF0_LEVEL)
        };
        let mut rl_mode = false;
        let mut cur = 0;
        let mut zeros = 0;
        while (transmit_num_vec || !rl_mode) && cur + 3 < ch.num_vec_coeffs {
            let mut vals = [0u32; 4];
            let idx = t.vec4.read(gb);
            if idx < 0 {
                for i in [0, 2] {
                    let idx = t.vec2.read(gb);
                    if idx < 0 {
                        for v in &mut vals[i..i + 2] {
                            let mut x = t.vec1.read(gb) as u32;
                            if x == VEC1_TABLE.len() as u32 - 1 {
                                x = x.wrapping_add(large_val(gb));
                            }
                            *v = x;
                        }
                    } else {
                        vals[i] = (idx >> 4) as u32;
                        vals[i + 1] = (idx & 0xF) as u32;
                    }
                }
            } else {
                let idx = idx as u32;
                vals = [idx >> 12, (idx >> 8) & 0xF, (idx >> 4) & 0xF, idx & 0xF];
            }
            for v in vals {
                ch.out[base + cur] = if v != 0 {
                    zeros = 0;
                    if gb.bit() {
                        v as f32
                    } else {
                        -(v as f32)
                    }
                } else {
                    zeros += 1;
                    rl_mode |= zeros > subframe_len >> 8;
                    0.0
                };
                cur += 1;
            }
        }
        if cur < subframe_len {
            ch.out[base + cur..base + subframe_len].fill(0.0);
            let mask = subframe_len - 1;
            let mut offset = cur;
            while offset < subframe_len {
                let code = vlc.read(gb);
                if code > 1 {
                    let code = code as usize;
                    offset += run[code] as usize;
                    let l = level[code] as f32;
                    ch.out[base + (offset & mask)] = if gb.bit() { l } else { -l };
                } else if code == 1 {
                    break;
                } else {
                    let l = large_val(gb) as f32;
                    if gb.bit() {
                        if gb.bit() {
                            ensure!(!gb.bit(), "broken escape sequence");
                            offset += gb.read(esc_len) as usize + 4;
                        } else {
                            offset += gb.read(2) as usize + 1;
                        }
                    }
                    ch.out[base + (offset & mask)] = if gb.bit() { l } else { -l };
                }
                offset += 1;
            }
            if offset > subframe_len {
                bail!("spectral run-level overflow ({offset} > {subframe_len})");
            }
        }
        Ok(())
    }

    fn inverse_channel_transform(&mut self) {
        let sfb = &self.sfb_offsets[self.table_idx];
        for g in &self.groups {
            if !g.transform || g.channels.len() != 2 {
                continue;
            }
            let (a, b) = (g.channels[0], g.channels[1]);
            let (ca, cb) = (self.channel[a].coeffs, self.channel[b].coeffs);
            for band in 0..self.num_bands {
                let (lo, hi) = (sfb[band], sfb[band + 1].min(self.subframe_len));
                for y in lo..hi {
                    let d0 = self.channel[a].out[ca + y];
                    let d1 = self.channel[b].out[cb + y];
                    let (o0, o1) = if g.transform_band[band] {
                        (
                            d0 * g.matrix[0] + d1 * g.matrix[1],
                            d0 * g.matrix[2] + d1 * g.matrix[3],
                        )
                    } else {
                        (d0 * (181.0 / 128.0), d1 * (181.0 / 128.0))
                    };
                    self.channel[a].out[ca + y] = o0;
                    self.channel[b].out[cb + y] = o1;
                }
            }
        }
    }

    /// Sine window and overlap-add with the previous block (FFmpeg `vector_fmul_window`).
    fn window(&mut self) {
        let t = tables();
        for i in 0..self.channels_for_cur_subframe {
            let ch = &mut self.channel[self.channel_indexes[i]];
            let mut winlen = ch.prev_block_len;
            let mut start = ch.coeffs - winlen / 2;
            if self.subframe_len < winlen {
                start += (winlen - self.subframe_len) / 2;
                winlen = self.subframe_len;
            }
            let win = &t.windows[log2(winlen) - log2(MIN_SUBFRAME)];
            let len = winlen / 2;
            let base = start + len;
            for k in 0..len {
                let (pi, pj) = (start + k, base + len - 1 - k);
                let (s0, s1) = (ch.out[pi], ch.out[pj]);
                let (wi, wj) = (win[k], win[2 * len - 1 - k]);
                ch.out[pi] = s0 * wj - s1 * wi;
                ch.out[pj] = s0 * wi + s1 * wj;
            }
            ch.prev_block_len = self.subframe_len;
        }
    }
}

// Tables from FFmpeg `wmaprodata.h` (generated by `private/tmp/xma/gentables.py`). Pairs are
// `[symbol, code length]`.

const CRITICAL_FREQ: [u16; 28] = [
    100, 200, 300, 400, 510, 630, 770, 920, 1080, 1270, 1480, 1720, 2000, 2320, 2700, 3150, 3700,
    4400, 5300, 6400, 7700, 9500, 12000, 15500, 20675, 28575, 41375, 63875,
];

const SCALE_TABLE: [[u16; 2]; 121] = [
    [58, 5],
    [64, 6],
    [66, 7],
    [65, 7],
    [62, 5],
    [63, 6],
    [68, 9],
    [69, 10],
    [54, 15],
    [19, 19],
    [20, 19],
    [21, 19],
    [22, 19],
    [23, 19],
    [24, 19],
    [25, 19],
    [26, 19],
    [27, 19],
    [28, 19],
    [29, 19],
    [30, 19],
    [31, 19],
    [32, 19],
    [33, 19],
    [34, 19],
    [17, 19],
    [36, 19],
    [37, 19],
    [38, 19],
    [39, 19],
    [40, 19],
    [41, 19],
    [42, 19],
    [43, 19],
    [44, 19],
    [45, 19],
    [46, 19],
    [47, 19],
    [48, 19],
    [49, 19],
    [50, 19],
    [51, 19],
    [52, 19],
    [15, 19],
    [16, 19],
    [14, 19],
    [13, 19],
    [12, 19],
    [11, 19],
    [10, 19],
    [0, 19],
    [9, 19],
    [8, 19],
    [7, 19],
    [6, 19],
    [5, 19],
    [4, 19],
    [55, 13],
    [70, 13],
    [3, 19],
    [2, 19],
    [1, 19],
    [35, 19],
    [71, 19],
    [72, 19],
    [73, 19],
    [74, 19],
    [75, 19],
    [76, 19],
    [77, 19],
    [78, 19],
    [79, 19],
    [80, 19],
    [81, 19],
    [82, 19],
    [83, 19],
    [84, 19],
    [85, 19],
    [86, 19],
    [87, 19],
    [88, 19],
    [89, 19],
    [90, 19],
    [91, 19],
    [92, 19],
    [93, 19],
    [94, 19],
    [95, 19],
    [96, 19],
    [97, 19],
    [98, 19],
    [99, 19],
    [100, 19],
    [101, 19],
    [102, 19],
    [103, 19],
    [104, 19],
    [105, 19],
    [106, 19],
    [107, 19],
    [108, 19],
    [109, 19],
    [110, 19],
    [111, 19],
    [112, 19],
    [113, 19],
    [114, 19],
    [115, 19],
    [116, 19],
    [117, 19],
    [118, 19],
    [119, 19],
    [120, 19],
    [18, 18],
    [53, 16],
    [56, 11],
    [57, 8],
    [67, 7],
    [61, 3],
    [59, 2],
    [60, 1],
];

const SCALE_RL_TABLE: [[u16; 2]; 120] = [
    [103, 7],
    [80, 11],
    [60, 11],
    [18, 10],
    [56, 10],
    [21, 12],
    [90, 12],
    [58, 11],
    [27, 11],
    [69, 12],
    [84, 15],
    [48, 15],
    [86, 14],
    [47, 13],
    [19, 10],
    [32, 9],
    [78, 6],
    [5, 5],
    [28, 4],
    [53, 5],
    [9, 7],
    [31, 8],
    [38, 8],
    [10, 7],
    [88, 11],
    [25, 12],
    [105, 12],
    [118, 11],
    [23, 12],
    [82, 14],
    [98, 16],
    [110, 16],
    [108, 15],
    [93, 13],
    [68, 10],
    [72, 12],
    [97, 12],
    [81, 12],
    [42, 12],
    [64, 8],
    [4, 4],
    [1, 2],
    [7, 6],
    [14, 7],
    [0, 9],
    [55, 9],
    [61, 9],
    [117, 10],
    [24, 12],
    [44, 12],
    [67, 12],
    [70, 16],
    [99, 18],
    [96, 21],
    [95, 21],
    [2, 21],
    [77, 21],
    [52, 21],
    [111, 21],
    [102, 20],
    [101, 17],
    [46, 15],
    [73, 15],
    [109, 15],
    [51, 14],
    [92, 14],
    [30, 7],
    [11, 7],
    [66, 7],
    [15, 8],
    [16, 8],
    [116, 9],
    [65, 9],
    [57, 10],
    [59, 10],
    [115, 9],
    [12, 7],
    [35, 9],
    [17, 9],
    [41, 9],
    [20, 11],
    [91, 11],
    [26, 12],
    [75, 15],
    [45, 15],
    [107, 14],
    [83, 14],
    [100, 15],
    [89, 15],
    [43, 11],
    [62, 9],
    [37, 9],
    [104, 8],
    [6, 5],
    [39, 8],
    [40, 9],
    [34, 9],
    [79, 7],
    [8, 6],
    [63, 6],
    [87, 12],
    [94, 14],
    [49, 14],
    [50, 13],
    [22, 11],
    [119, 10],
    [33, 9],
    [36, 9],
    [113, 11],
    [106, 12],
    [112, 13],
    [71, 15],
    [85, 15],
    [74, 14],
    [76, 10],
    [114, 7],
    [29, 5],
    [54, 6],
    [13, 6],
    [3, 2],
];

const SCALE_RL_RUN: [u8; 120] = [
    0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
    24, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 0, 1,
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 0, 1, 2, 3,
    4, 5, 6, 7, 8, 9, 10, 0, 1, 0, 1, 0, 1,
];

const SCALE_RL_LEVEL: [u8; 120] = [
    0, 0, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 7, 7, 8, 8, 9, 9,
];

const COEF0_LENS: [u8; 272] = [
    2, 9, 14, 14, 13, 12, 13, 14, 15, 15, 12, 10, 10, 10, 13, 14, 15, 15, 12, 11, 13, 14, 14, 13,
    15, 15, 14, 12, 12, 8, 10, 10, 15, 15, 14, 13, 14, 14, 13, 15, 20, 20, 19, 21, 21, 20, 19, 17,
    17, 18, 18, 15, 15, 13, 12, 14, 15, 15, 14, 15, 15, 12, 11, 6, 7, 8, 9, 13, 13, 13, 14, 14, 11,
    10, 7, 8, 14, 14, 14, 14, 12, 13, 13, 12, 12, 12, 11, 9, 13, 14, 14, 12, 11, 11, 11, 9, 8, 7,
    14, 15, 15, 14, 14, 12, 13, 15, 16, 17, 17, 14, 12, 12, 12, 15, 15, 14, 14, 14, 13, 13, 9, 9,
    11, 11, 10, 7, 6, 13, 15, 15, 14, 14, 14, 13, 14, 15, 15, 13, 14, 14, 14, 14, 10, 9, 10, 10,
    11, 11, 10, 8, 9, 13, 14, 14, 12, 11, 14, 15, 15, 13, 12, 14, 14, 14, 14, 13, 14, 14, 3, 5, 8,
    10, 10, 15, 15, 14, 14, 16, 16, 15, 12, 11, 11, 11, 7, 8, 8, 9, 12, 13, 13, 12, 14, 15, 15, 13,
    10, 11, 11, 13, 14, 14, 13, 14, 14, 11, 10, 13, 15, 15, 14, 12, 11, 4, 6, 6, 8, 12, 12, 12, 13,
    13, 12, 13, 13, 14, 14, 13, 13, 13, 9, 7, 9, 11, 14, 14, 13, 14, 14, 13, 10, 8, 7, 5, 9, 12,
    13, 14, 15, 15, 12, 12, 10, 14, 14, 13, 12, 13, 14, 14, 12, 13, 13, 12, 12, 12, 9, 7, 6, 3, 4,
    4,
];

const COEF0_SYMS: [u16; 272] = [
    2, 25, 111, 94, 69, 58, 87, 93, 136, 135, 59, 37, 34, 36, 82, 182, 120, 138, 195, 45, 168, 216,
    178, 86, 140, 219, 186, 162, 239, 18, 156, 35, 127, 236, 109, 85, 180, 253, 88, 147, 268, 264,
    256, 266, 270, 262, 260, 248, 246, 252, 258, 137, 189, 230, 64, 179, 146, 208, 101, 118, 238,
    163, 46, 9, 153, 0, 26, 247, 169, 76, 202, 131, 194, 38, 13, 19, 132, 106, 191, 97, 65, 198,
    77, 62, 66, 164, 48, 27, 81, 183, 102, 60, 47, 49, 159, 227, 20, 14, 112, 263, 144, 217, 104,
    63, 79, 209, 269, 250, 254, 203, 241, 196, 61, 220, 148, 124, 185, 100, 80, 78, 193, 28, 50,
    235, 41, 1, 10, 171, 226, 150, 103, 114, 115, 170, 105, 211, 149, 249, 108, 188, 107, 255, 231,
    155, 42, 40, 55, 160, 39, 21, 29, 215, 234, 184, 228, 51, 116, 142, 145, 172, 165, 181, 130,
    113, 117, 89, 128, 204, 3, 7, 154, 157, 43, 141, 265, 133, 225, 271, 244, 221, 74, 54, 56, 52,
    15, 222, 22, 30, 83, 199, 173, 73, 123, 210, 143, 175, 44, 53, 237, 174, 139, 134, 110, 218,
    129, 161, 213, 177, 267, 151, 125, 67, 223, 5, 11, 192, 23, 214, 243, 166, 200, 176, 68, 224,
    187, 257, 261, 232, 96, 251, 31, 16, 32, 57, 207, 121, 91, 126, 119, 99, 158, 24, 212, 8, 33,
    70, 92, 205, 240, 242, 75, 197, 233, 259, 190, 98, 71, 201, 122, 206, 72, 90, 95, 84, 167, 245,
    229, 17, 12, 4, 152, 6,
];

const COEF1_TABLE: [[u16; 2]; 244] = [
    [2, 2],
    [3, 3],
    [102, 3],
    [4, 4],
    [148, 6],
    [134, 9],
    [171, 10],
    [18, 10],
    [11, 8],
    [159, 8],
    [14, 9],
    [156, 14],
    [235, 15],
    [61, 15],
    [38, 13],
    [153, 13],
    [48, 14],
    [49, 14],
    [23, 11],
    [203, 13],
    [208, 19],
    [204, 19],
    [129, 18],
    [94, 17],
    [87, 16],
    [62, 15],
    [174, 15],
    [147, 15],
    [29, 12],
    [191, 12],
    [64, 15],
    [65, 15],
    [146, 14],
    [164, 13],
    [142, 5],
    [132, 4],
    [103, 5],
    [154, 7],
    [165, 9],
    [181, 11],
    [109, 12],
    [30, 12],
    [86, 16],
    [92, 16],
    [239, 15],
    [138, 14],
    [39, 13],
    [50, 14],
    [115, 15],
    [238, 21],
    [228, 21],
    [236, 21],
    [222, 21],
    [216, 20],
    [226, 20],
    [196, 18],
    [192, 17],
    [120, 16],
    [221, 14],
    [51, 14],
    [24, 11],
    [143, 8],
    [7, 6],
    [9, 7],
    [152, 10],
    [136, 12],
    [160, 12],
    [241, 15],
    [66, 15],
    [168, 14],
    [219, 14],
    [113, 14],
    [193, 12],
    [19, 10],
    [173, 10],
    [105, 8],
    [149, 9],
    [15, 9],
    [205, 13],
    [207, 13],
    [125, 17],
    [190, 17],
    [182, 16],
    [68, 15],
    [70, 15],
    [67, 15],
    [137, 13],
    [31, 12],
    [223, 14],
    [116, 15],
    [210, 19],
    [220, 19],
    [198, 18],
    [126, 17],
    [88, 16],
    [41, 13],
    [25, 11],
    [40, 13],
    [73, 15],
    [243, 15],
    [53, 14],
    [195, 12],
    [183, 11],
    [225, 14],
    [52, 14],
    [71, 15],
    [121, 16],
    [89, 16],
    [170, 14],
    [55, 14],
    [69, 15],
    [83, 15],
    [209, 13],
    [108, 11],
    [32, 12],
    [54, 14],
    [122, 16],
    [184, 16],
    [176, 15],
    [42, 13],
    [12, 8],
    [161, 8],
    [6, 5],
    [167, 9],
    [106, 9],
    [20, 10],
    [145, 12],
    [111, 13],
    [43, 13],
    [26, 11],
    [175, 10],
    [107, 10],
    [34, 12],
    [33, 12],
    [197, 12],
    [74, 15],
    [128, 17],
    [232, 20],
    [212, 20],
    [224, 19],
    [202, 18],
    [90, 16],
    [57, 14],
    [227, 14],
    [97, 16],
    [93, 16],
    [140, 15],
    [185, 11],
    [27, 11],
    [16, 9],
    [158, 11],
    [211, 13],
    [56, 14],
    [117, 15],
    [72, 15],
    [166, 13],
    [91, 16],
    [95, 16],
    [80, 15],
    [101, 16],
    [194, 17],
    [127, 17],
    [82, 15],
    [21, 10],
    [144, 10],
    [177, 10],
    [151, 6],
    [10, 7],
    [157, 7],
    [8, 6],
    [5, 4],
    [13, 8],
    [0, 9],
    [213, 13],
    [46, 13],
    [199, 12],
    [35, 12],
    [162, 12],
    [135, 10],
    [169, 9],
    [45, 13],
    [59, 14],
    [114, 14],
    [44, 13],
    [188, 16],
    [186, 16],
    [75, 15],
    [79, 15],
    [118, 15],
    [187, 11],
    [112, 13],
    [139, 14],
    [178, 15],
    [81, 15],
    [110, 12],
    [28, 11],
    [163, 8],
    [133, 6],
    [104, 6],
    [17, 9],
    [22, 10],
    [229, 14],
    [172, 14],
    [217, 13],
    [201, 12],
    [36, 12],
    [218, 20],
    [242, 22],
    [240, 22],
    [234, 21],
    [230, 19],
    [206, 18],
    [200, 18],
    [214, 18],
    [130, 17],
    [131, 17],
    [141, 15],
    [84, 15],
    [76, 15],
    [215, 13],
    [58, 14],
    [231, 14],
    [233, 14],
    [180, 15],
    [77, 15],
    [37, 12],
    [189, 11],
    [179, 10],
    [155, 10],
    [47, 13],
    [96, 16],
    [99, 16],
    [119, 15],
    [63, 14],
    [237, 14],
    [78, 15],
    [85, 15],
    [60, 14],
    [98, 16],
    [100, 16],
    [124, 16],
    [123, 16],
    [150, 11],
    [1, 7],
];

const COEF0_RUN: [u16; 272] = [
    0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
    25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72,
    73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96,
    97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115,
    116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134,
    135, 136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 0, 1, 2, 3, 4, 5, 6,
    7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 32, 33, 34, 35, 36, 37, 38, 39, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
    1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
];

const COEF0_LEVEL: [u8; 272] = [
    0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5,
    5, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17,
    17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26, 27, 27, 28,
];

const COEF1_RUN: [u16; 244] = [
    0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
    25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72,
    73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96,
    97, 98, 99, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
    23, 24, 25, 26, 27, 28, 29, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4, 5, 0, 1, 2, 0, 1, 2,
    0, 1, 2, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
    1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
    1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0, 0,
];

const COEF1_LEVEL: [u8; 244] = [
    0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 5, 5, 5, 6, 6, 6, 7, 7, 7, 8, 8, 9,
    9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21,
    21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26, 27, 27, 28, 28, 29, 29, 30, 30, 31, 31, 32, 32, 33,
    33, 34, 34, 35, 35, 36, 36, 37, 37, 38, 38, 39, 39, 40, 40, 41, 41, 42, 42, 43, 43, 44, 44, 45,
    45, 46, 46, 47, 47, 48, 48, 49, 49, 50, 51, 52,
];

const VEC4_LENS: [u8; 127] = [
    1, 6, 8, 10, 10, 10, 10, 8, 8, 10, 10, 9, 8, 8, 9, 12, 12, 11, 12, 12, 11, 9, 9, 8, 8, 9, 9, 8,
    8, 9, 9, 12, 12, 12, 14, 14, 13, 11, 11, 9, 8, 9, 9, 11, 11, 10, 9, 8, 6, 6, 6, 6, 6, 6, 11,
    11, 10, 11, 11, 10, 10, 11, 11, 9, 7, 6, 7, 7, 6, 6, 6, 5, 7, 11, 11, 10, 9, 8, 6, 9, 9, 10,
    10, 9, 8, 8, 6, 6, 6, 8, 8, 9, 12, 12, 11, 10, 8, 8, 8, 10, 10, 10, 10, 9, 9, 8, 10, 11, 11, 9,
    8, 8, 8, 9, 9, 10, 11, 12, 12, 9, 9, 9, 8, 8, 7, 7, 7,
];

const VEC4_SYMS: [u16; 127] = [
    0, 4370, 275, 8195, 4146, 12545, 8225, 290, 4625, 515, 20, 8706, 8210, 4355, 4131, 16385, 5121,
    8961, 321, 1041, 51, 4641, 546, 4610, 530, 513, 8451, 4385, 4130, 33, 8211, 5, 66, 4161, 1281,
    81, 6, 801, 8196, 8481, 8449, 4611, 531, 561, 769, 12290, 8226, 19, 4097, 2, 4369, 274, 4354,
    4114, 12291, 16641, 12305, 49, 12321, 260, 4100, 516, 21, 12546, 8466, 4353, 4371, 4626, 257,
    18, 17, 1, 4386, 8241, 771, 4865, 8705, 8194, 4098, 12561, 276, 50, 785, 4116, 8209, 4099, 273,
    4113, 258, 259, 4609, 35, 1026, 1025, 16401, 305, 34, 529, 289, 770, 12289, 4, 4145, 4356,
    12306, 8193, 12801, 261, 16386, 4881, 3, 514, 4129, 545, 306, 36, 4101, 65, 20481, 786, 4401,
    4866, 8721, 291, 8450, 8465, 4115,
];

const VEC2_TABLE: [[u16; 2]; 137] = [
    [19, 5],
    [165, 10],
    [211, 11],
    [46, 11],
    [75, 10],
    [177, 11],
    [12, 11],
    [86, 8],
    [83, 7],
    [38, 7],
    [133, 9],
    [178, 10],
    [28, 10],
    [104, 9],
    [73, 9],
    [35, 5],
    [52, 6],
    [113, 9],
    [8, 9],
    [101, 8],
    [69, 7],
    [0, 3],
    [71, 8],
    [119, 9],
    [91, 10],
    [179, 10],
    [114, 8],
    [166, 10],
    [10, 10],
    [44, 10],
    [145, 10],
    [66, 6],
    [21, 6],
    [24, 8],
    [146, 9],
    [26, 9],
    [65, 7],
    [5, 7],
    [226, 11],
    [225, 12],
    [15, 12],
    [180, 10],
    [147, 9],
    [115, 8],
    [40, 8],
    [89, 9],
    [134, 9],
    [84, 7],
    [54, 7],
    [42, 9],
    [60, 10],
    [31, 11],
    [193, 11],
    [181, 10],
    [76, 10],
    [148, 9],
    [37, 6],
    [67, 6],
    [33, 6],
    [3, 6],
    [17, 6],
    [2, 6],
    [102, 8],
    [87, 8],
    [116, 8],
    [56, 8],
    [50, 5],
    [20, 5],
    [120, 9],
    [58, 9],
    [29, 10],
    [194, 10],
    [135, 9],
    [97, 8],
    [7, 8],
    [105, 9],
    [13, 11],
    [241, 12],
    [16, 12],
    [45, 10],
    [149, 9],
    [74, 9],
    [98, 7],
    [23, 7],
    [85, 7],
    [70, 7],
    [195, 10],
    [161, 10],
    [129, 9],
    [72, 8],
    [51, 5],
    [36, 5],
    [117, 8],
    [61, 10],
    [11, 10],
    [162, 9],
    [1, 7],
    [4, 6],
    [49, 6],
    [68, 6],
    [9, 9],
    [27, 9],
    [130, 8],
    [39, 7],
    [53, 6],
    [99, 7],
    [25, 8],
    [150, 9],
    [90, 9],
    [103, 8],
    [163, 9],
    [196, 10],
    [210, 10],
    [136, 9],
    [121, 9],
    [41, 8],
    [131, 8],
    [43, 9],
    [164, 9],
    [118, 8],
    [88, 8],
    [81, 7],
    [6, 7],
    [55, 7],
    [59, 9],
    [30, 10],
    [209, 11],
    [14, 11],
    [151, 9],
    [106, 9],
    [82, 6],
    [22, 6],
    [100, 7],
    [132, 8],
    [57, 8],
    [18, 4],
    [34, 4],
];

const VEC1_TABLE: [[u16; 2]; 101] = [
    [7, 5],
    [32, 8],
    [59, 10],
    [60, 10],
    [83, 11],
    [82, 11],
    [62, 10],
    [33, 8],
    [45, 9],
    [61, 10],
    [84, 11],
    [85, 11],
    [1, 6],
    [13, 5],
    [19, 6],
    [25, 7],
    [34, 8],
    [46, 9],
    [47, 9],
    [14, 5],
    [6, 5],
    [64, 10],
    [87, 11],
    [86, 11],
    [63, 10],
    [88, 11],
    [90, 11],
    [35, 8],
    [26, 7],
    [0, 7],
    [48, 9],
    [65, 10],
    [66, 10],
    [36, 8],
    [15, 5],
    [20, 6],
    [91, 11],
    [89, 11],
    [67, 10],
    [49, 9],
    [50, 9],
    [69, 10],
    [92, 11],
    [93, 11],
    [27, 7],
    [5, 5],
    [37, 8],
    [68, 10],
    [71, 10],
    [51, 9],
    [52, 9],
    [70, 10],
    [94, 11],
    [96, 11],
    [38, 8],
    [21, 6],
    [16, 5],
    [4, 5],
    [28, 7],
    [53, 9],
    [95, 11],
    [97, 11],
    [73, 10],
    [39, 8],
    [29, 7],
    [72, 10],
    [98, 11],
    [99, 11],
    [54, 9],
    [40, 8],
    [22, 6],
    [30, 7],
    [55, 9],
    [74, 10],
    [76, 10],
    [56, 9],
    [75, 10],
    [77, 10],
    [17, 5],
    [3, 5],
    [23, 6],
    [41, 8],
    [57, 9],
    [78, 10],
    [79, 10],
    [31, 7],
    [10, 4],
    [9, 4],
    [100, 5],
    [2, 5],
    [11, 4],
    [8, 4],
    [18, 5],
    [42, 8],
    [58, 9],
    [80, 10],
    [81, 10],
    [43, 8],
    [44, 8],
    [24, 6],
    [12, 4],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_complete_prefix_codes() {
        let t = tables();
        for (name, vlc) in [
            ("sf", &t.sf),
            ("sf_rl", &t.sf_rl),
            ("coef0", &t.coef[0]),
            ("coef1", &t.coef[1]),
            ("vec4", &t.vec4),
            ("vec2", &t.vec2),
            ("vec1", &t.vec1),
        ] {
            let (last, len, _) = *vlc.codes.last().unwrap();
            assert_eq!(
                last as u64 + (1u64 << (32 - len)),
                1u64 << 32,
                "{name} is not a complete code"
            );
        }
    }

    #[test]
    fn imdct_matches_definition() {
        let t = tables();
        let n = 128;
        let mut x = vec![0f32; n];
        x[3] = 1.0;
        let out: Vec<f32> = (0..n)
            .map(|j| {
                t.imdct[0][j * n..j * n + n]
                    .iter()
                    .zip(&x)
                    .map(|(a, b)| a * b)
                    .sum()
            })
            .collect();
        let scale = -1.0 / 64.0 / 32768.0;
        for (j, v) in out.iter().enumerate() {
            let want =
                scale * (std::f64::consts::PI / n as f64 * (j as f64 + n as f64 + 0.5) * 3.5).cos();
            assert!((*v as f64 - want).abs() < 1e-9, "{j}: {v} vs {want}");
        }
    }
}
