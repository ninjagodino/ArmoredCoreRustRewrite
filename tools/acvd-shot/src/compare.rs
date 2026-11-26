//! Numeric comparison for two screenshots. The 360 guest shot is the reference; the rewrite
//! `--shot` is the candidate. Reports tone/metrics per tile, optionally aligns a small translation
//! and writes an amplified diff plus a reference | candidate | diff strip.

use image::{imageops, Rgb, RgbImage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Stretch,
    Letterbox,
    Crop,
}

impl Fit {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stretch" => Some(Fit::Stretch),
            "letterbox" => Some(Fit::Letterbox),
            "crop" => Some(Fit::Crop),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Fit::Stretch => "stretch",
            Fit::Letterbox => "letterbox",
            Fit::Crop => "crop",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn parse(s: &str) -> Option<Self> {
        let v: Vec<u32> = s
            .split(',')
            .map(|p| p.trim().parse().ok())
            .collect::<Option<_>>()?;
        (v.len() == 4).then(|| Rect {
            x: v[0],
            y: v[1],
            w: v[2],
            h: v[3],
        })
    }

    pub fn clamped(self, w: u32, h: u32) -> Self {
        let x = self.x.min(w.saturating_sub(1));
        let y = self.y.min(h.saturating_sub(1));
        Rect {
            x,
            y,
            w: self.w.min(w - x).max(1),
            h: self.h.min(h - y).max(1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub fit: Fit,
    pub crop: Option<Rect>,
    pub ignore: Vec<Rect>,
    pub align: u32,
    /// Per-channel difference counted as "over" (0-255).
    pub max_diff: u8,
    /// Pass if the mean absolute error is at most this (0-1).
    pub max_mae: f64,
    /// Pass if at most this fraction of pixels is over `max_diff` (0-1).
    pub max_over: f64,
    /// Diff image amplification.
    pub diff_gain: f32,
    /// Tile grid side count.
    pub tiles: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fit: Fit::Stretch,
            crop: None,
            ignore: Vec::new(),
            align: 0,
            max_diff: 8,
            max_mae: 0.02,
            max_over: 0.10,
            diff_gain: 4.0,
            tiles: 8,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Metrics {
    pub pixels: u64,
    pub mae: f64,
    pub rmse: f64,
    pub over: f64,
    pub ref_luma: f64,
    pub cand_luma: f64,
    pub luma_bias: f64,
    pub gradient: f64,
}

#[derive(Clone, Debug)]
pub struct TileDiff {
    pub tx: u32,
    pub ty: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub mae: f64,
    /// Mean reference luma minus candidate luma in this tile (0-1).
    pub bias: f64,
}

pub struct Report {
    pub reference_size: (u32, u32),
    pub candidate_size: (u32, u32),
    pub width: u32,
    pub height: u32,
    pub shift: (i32, i32),
    pub metrics: Metrics,
    pub tiles: Vec<TileDiff>,
    pub pass: bool,
    pub diff: RgbImage,
    pub side: RgbImage,
}

pub fn load(path: &std::path::Path) -> anyhow::Result<RgbImage> {
    Ok(image::open(path)?.to_rgb8())
}

/// Resize `src` onto a `w`x`h` canvas. `Stretch` ignores aspect; `Letterbox` fits and pads black;
/// `Crop` fills and centre-crops.
pub fn fit_candidate(src: &RgbImage, w: u32, h: u32, fit: Fit) -> RgbImage {
    let (sw, sh) = (src.width().max(1), src.height().max(1));
    if (sw, sh) == (w, h) {
        return src.clone();
    }
    match fit {
        Fit::Stretch => imageops::resize(src, w.max(1), h.max(1), imageops::FilterType::Triangle),
        Fit::Letterbox => {
            let s = (w as f64 / sw as f64).min(h as f64 / sh as f64);
            let nw = ((sw as f64 * s).round() as u32).clamp(1, w.max(1));
            let nh = ((sh as f64 * s).round() as u32).clamp(1, h.max(1));
            let resized = imageops::resize(src, nw, nh, imageops::FilterType::Triangle);
            let mut out = RgbImage::from_pixel(w.max(1), h.max(1), Rgb([0, 0, 0]));
            imageops::replace(
                &mut out,
                &resized,
                ((w - nw) / 2) as i64,
                ((h - nh) / 2) as i64,
            );
            out
        }
        Fit::Crop => {
            let s = (w as f64 / sw as f64).max(h as f64 / sh as f64);
            let nw = ((sw as f64 * s).round() as u32).max(w.max(1));
            let nh = ((sh as f64 * s).round() as u32).max(h.max(1));
            let resized = imageops::resize(src, nw, nh, imageops::FilterType::Triangle);
            imageops::crop_imm(&resized, (nw - w) / 2, (nh - h) / 2, w.max(1), h.max(1)).to_image()
        }
    }
}

fn crop_to(img: &RgbImage, rect: Rect) -> RgbImage {
    imageops::crop_imm(img, rect.x, rect.y, rect.w, rect.h).to_image()
}

fn luma(p: Rgb<u8>) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

fn luma_image(img: &RgbImage) -> Vec<f32> {
    img.pixels().map(|p| luma(*p)).collect()
}

/// Best integer translation of `candidate` onto `reference`, searched on a strided luma sample.
/// Ties keep `(0, 0)` so a flat frame does not invent a shift.
pub fn best_shift(reference: &RgbImage, candidate: &RgbImage, radius: u32) -> (i32, i32) {
    let (w, h) = (reference.width() as usize, reference.height() as usize);
    if radius == 0
        || w == 0
        || h == 0
        || (candidate.width() as usize, candidate.height() as usize) != (w, h)
    {
        return (0, 0);
    }
    let rl = luma_image(reference);
    let cl = luma_image(candidate);
    let stride = (w.min(h) / 200).max(1);
    let r = radius as i32;
    let mut best = ((0, 0), f64::INFINITY);
    for dy in -r..=r {
        for dx in -r..=r {
            let mut sum = 0.0;
            let mut n = 0u64;
            for y in (0..h).step_by(stride) {
                let sy = y as i32 + dy;
                if sy < 0 || sy >= h as i32 {
                    continue;
                }
                for x in (0..w).step_by(stride) {
                    let sx = x as i32 + dx;
                    if sx < 0 || sx >= w as i32 {
                        continue;
                    }
                    sum += (rl[y * w + x] - cl[sy as usize * w + sx as usize]).abs() as f64;
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            let score = sum / n as f64;
            if score + 1e-12 < best.1 {
                best = ((dx, dy), score);
            }
        }
    }
    best.0
}

pub fn compare(reference: &RgbImage, candidate: &RgbImage, opts: &Options) -> Report {
    let (rw, rh) = reference.dimensions();
    let fitted = fit_candidate(candidate, rw, rh, opts.fit);

    let (reference, fitted) = match opts.crop {
        Some(r) => {
            let r = r.clamped(rw, rh);
            (crop_to(reference, r), crop_to(&fitted, r))
        }
        None => (reference.clone(), fitted),
    };

    let (w, h) = reference.dimensions();
    let n = (w as usize) * (h as usize);
    let shift = if opts.align > 0 {
        best_shift(&reference, &fitted, opts.align)
    } else {
        (0, 0)
    };
    let (dx, dy) = shift;

    let mut ignored = vec![false; n];
    for rect in &opts.ignore {
        let rect = rect.clamped(w, h);
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                ignored[y as usize * w as usize + x as usize] = true;
            }
        }
    }

    let mut valid = vec![true; n];
    let mut shifted = RgbImage::from_pixel(w, h, Rgb([0, 0, 0]));
    for y in 0..h {
        for x in 0..w {
            let i = y as usize * w as usize + x as usize;
            let sx = x as i64 + dx as i64;
            let sy = y as i64 + dy as i64;
            if ignored[i] || sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                valid[i] = false;
                continue;
            }
            shifted.put_pixel(x, y, *fitted.get_pixel(sx as u32, sy as u32));
        }
    }

    let ref_l = luma_image(&reference);
    let cand_l = luma_image(&shifted);

    let thresh = opts.max_diff as f32 / 255.0;
    let mut sum_abs = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut over = 0u64;
    let mut sum_ref = 0.0f64;
    let mut sum_cand = 0.0f64;
    let mut sum_grad = 0.0f64;
    let mut grad_n = 0u64;
    let mut pixels = 0u64;
    let mut diff = RgbImage::from_pixel(w, h, Rgb([0, 0, 0]));

    let tiles = opts.tiles.max(1);
    let ntiles = (tiles * tiles) as usize;
    let mut tile_sum = vec![0.0f64; ntiles];
    let mut tile_bias = vec![0.0f64; ntiles];
    let mut tile_n = vec![0u64; ntiles];

    for y in 0..h {
        for x in 0..w {
            let i = y as usize * w as usize + x as usize;
            if !valid[i] {
                continue;
            }
            let a = *reference.get_pixel(x, y);
            let b = *shifted.get_pixel(x, y);
            let d = [
                (a[0] as f32 - b[0] as f32) / 255.0,
                (a[1] as f32 - b[1] as f32) / 255.0,
                (a[2] as f32 - b[2] as f32) / 255.0,
            ];
            let abs = (d[0].abs() + d[1].abs() + d[2].abs()) as f64 / 3.0;
            sum_abs += abs;
            sum_sq += ((ref_l[i] - cand_l[i]) as f64).powi(2);
            sum_ref += ref_l[i] as f64;
            sum_cand += cand_l[i] as f64;
            if d.iter().any(|v| v.abs() > thresh) {
                over += 1;
            }
            pixels += 1;

            let tx = (x * tiles / w).min(tiles - 1) as usize;
            let ty = (y * tiles / h).min(tiles - 1) as usize;
            let t = ty * tiles as usize + tx;
            tile_sum[t] += abs;
            tile_bias[t] += (ref_l[i] - cand_l[i]) as f64;
            tile_n[t] += 1;

            if x + 1 < w && valid[i + 1] {
                sum_grad +=
                    (((ref_l[i + 1] - ref_l[i]) - (cand_l[i + 1] - cand_l[i])).abs()) as f64;
                grad_n += 1;
            }
            if y + 1 < h {
                let j = i + w as usize;
                if valid[j] {
                    sum_grad += (((ref_l[j] - ref_l[i]) - (cand_l[j] - cand_l[i])).abs()) as f64;
                    grad_n += 1;
                }
            }

            let amp = |a: u8, b: u8| {
                (((a as f32 - b as f32).abs() / 255.0 * opts.diff_gain).min(1.0) * 255.0) as u8
            };
            diff.put_pixel(
                x,
                y,
                Rgb([amp(a[0], b[0]), amp(a[1], b[1]), amp(a[2], b[2])]),
            );
        }
    }

    let denom = pixels.max(1) as f64;
    let metrics = Metrics {
        pixels,
        mae: sum_abs / denom,
        rmse: (sum_sq / denom).sqrt(),
        over: over as f64 / denom,
        ref_luma: sum_ref / denom,
        cand_luma: sum_cand / denom,
        luma_bias: (sum_ref - sum_cand) / denom,
        gradient: if grad_n == 0 {
            0.0
        } else {
            sum_grad / grad_n as f64
        },
    };
    let pass = metrics.pixels > 0 && metrics.mae <= opts.max_mae && metrics.over <= opts.max_over;

    let mut tiles_out = Vec::new();
    for ty in 0..tiles {
        for tx in 0..tiles {
            let t = (ty * tiles + tx) as usize;
            if tile_n[t] == 0 {
                continue;
            }
            let x0 = tx * w / tiles;
            let y0 = ty * h / tiles;
            tiles_out.push(TileDiff {
                tx,
                ty,
                x: x0,
                y: y0,
                w: (tx + 1) * w / tiles - x0,
                h: (ty + 1) * h / tiles - y0,
                mae: tile_sum[t] / tile_n[t] as f64,
                bias: tile_bias[t] / tile_n[t] as f64,
            });
        }
    }
    tiles_out.sort_by(|a, b| b.mae.total_cmp(&a.mae));
    tiles_out.truncate(5);

    let mut side = RgbImage::new(w * 3 + 16, h);
    imageops::replace(&mut side, &reference, 0, 0);
    imageops::replace(&mut side, &shifted, w as i64 + 8, 0);
    imageops::replace(&mut side, &diff, 2 * w as i64 + 16, 0);

    Report {
        reference_size: (rw, rh),
        candidate_size: (candidate.width(), candidate.height()),
        width: w,
        height: h,
        shift,
        metrics,
        tiles: tiles_out,
        pass,
        diff,
        side,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> RgbImage {
        RgbImage::from_pixel(w, h, Rgb(rgb))
    }

    fn check(img: &RgbImage, x: u32, y: u32, rgb: [u8; 3]) {
        assert_eq!(img.get_pixel(x, y), &Rgb(rgb), "pixel {x},{y}");
    }

    #[test]
    fn identical_images_have_zero_error() {
        let a = solid(64, 32, [40, 80, 120]);
        let r = compare(&a, &a, &Options::default());
        assert_eq!(r.metrics.mae, 0.0);
        assert_eq!(r.metrics.over, 0.0);
        assert!(r.pass);
    }

    #[test]
    fn brightness_offset_is_measured_not_aligned_away() {
        let a = solid(32, 32, [100, 100, 100]);
        let b = solid(32, 32, [90, 90, 90]);
        let r = compare(&a, &b, &Options::default());
        assert!((r.metrics.luma_bias * 255.0 - 10.0).abs() < 1e-4);
        assert!((r.metrics.over - 1.0).abs() < 1e-9);
        assert!(!r.pass);
    }

    #[test]
    fn ignore_rect_excludes_pixels() {
        let a = solid(16, 16, [0, 0, 0]);
        let mut b = a.clone();
        for y in 0..8 {
            for x in 0..8 {
                b.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        let opts = Options {
            ignore: vec![Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            }],
            ..Options::default()
        };
        let r = compare(&a, &b, &opts);
        assert_eq!(r.metrics.pixels, 16 * 16 - 64);
        assert_eq!(r.metrics.over, 0.0);
    }

    #[test]
    fn alignment_recovers_a_small_shift() {
        let mut a = solid(48, 48, [0, 0, 0]);
        let mut b = solid(48, 48, [0, 0, 0]);
        for y in 8..24 {
            for x in 8..24 {
                a.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        for y in 10..26 {
            for x in 11..27 {
                b.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        assert_eq!(best_shift(&a, &b, 4), (3, 2));
        let opts = Options {
            align: 4,
            ..Options::default()
        };
        assert_eq!(compare(&a, &b, &opts).shift, (3, 2));
    }

    #[test]
    fn letterbox_centres_and_pads() {
        let src = solid(40, 20, [9, 9, 9]);
        let out = fit_candidate(&src, 40, 40, Fit::Letterbox);
        check(&out, 20, 0, [0, 0, 0]);
        check(&out, 20, 20, [9, 9, 9]);
    }

    #[test]
    fn tile_report_finds_the_hot_region() {
        let a = solid(64, 64, [0, 0, 0]);
        let mut b = a.clone();
        for y in 48..64 {
            for x in 48..64 {
                b.put_pixel(x, y, Rgb([80, 80, 80]));
            }
        }
        let opts = Options {
            tiles: 4,
            ..Options::default()
        };
        let r = compare(&a, &b, &opts);
        let worst = &r.tiles[0];
        assert_eq!((worst.tx, worst.ty), (3, 3));
    }
}
