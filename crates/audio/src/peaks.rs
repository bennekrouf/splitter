//! Min/max waveform peaks at several zoom levels, plus short-window loudness.

use serde::{Deserialize, Serialize};

/// Samples per bucket at level 0; each level above is `FACTOR` times coarser.
pub const BASE_BUCKET: u64 = 256;
const FACTOR: u64 = 4;
const LEVELS: usize = 5; // 256, 1k, 4k, 16k, 64k samples per bucket

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peaks {
    /// `levels[l][i]` = (min, max) over bucket `i` at level `l`, scaled to i16.
    pub levels: Vec<Vec<(i16, i16)>>,
}

impl Peaks {
    fn bucket(level: usize) -> u64 {
        BASE_BUCKET * FACTOR.pow(level as u32)
    }

    /// `n` columns of (min, max) in [-1, 1] covering samples `[start, end)`.
    pub fn columns(&self, start: u64, end: u64, n: usize) -> Vec<(f32, f32)> {
        if n == 0 || end <= start || self.levels.is_empty() {
            return vec![(0.0, 0.0); n];
        }
        let per_col = (end - start) as f64 / n as f64;
        let level = (0..self.levels.len()).rev().find(|&l| Self::bucket(l) as f64 <= per_col).unwrap_or(0);
        let bucket = Self::bucket(level) as f64;
        let data = &self.levels[level];

        (0..n)
            .map(|c| {
                let s0 = start as f64 + c as f64 * per_col;
                let s1 = s0 + per_col;
                let b0 = (s0 / bucket).floor() as usize;
                let b1 = ((s1 / bucket).ceil() as usize).max(b0 + 1).min(data.len());
                if b0 >= data.len() {
                    return (0.0, 0.0);
                }
                let (mut lo, mut hi) = (i16::MAX, i16::MIN);
                for &(mn, mx) in &data[b0..b1] {
                    lo = lo.min(mn);
                    hi = hi.max(mx);
                }
                (lo as f32 / 32767.0, hi as f32 / 32767.0)
            })
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Loudness {
    /// Samples per window (50 ms at the source rate).
    pub window: u64,
    /// RMS in dBFS per window, across all channels.
    pub db: Vec<f32>,
}

/// Streaming builder fed with interleaved frames during the scan pass.
pub struct AnalysisBuilder {
    channels: usize,
    min: f32,
    max: f32,
    in_bucket: u64,
    level0: Vec<(i16, i16)>,
    loud_window: u64,
    loud_acc: f64,
    loud_count: u64,
    loud: Vec<f32>,
}

impl AnalysisBuilder {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        Self {
            channels: channels.max(1),
            min: f32::MAX,
            max: f32::MIN,
            in_bucket: 0,
            level0: Vec::new(),
            loud_window: (sample_rate as u64 / 20).max(1),
            loud_acc: 0.0,
            loud_count: 0,
            loud: Vec::new(),
        }
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            let mut sq = 0.0f32;
            for &s in frame {
                self.min = self.min.min(s);
                self.max = self.max.max(s);
                sq += s * s;
            }
            self.in_bucket += 1;
            if self.in_bucket == BASE_BUCKET {
                self.flush_bucket();
            }
            self.loud_acc += (sq / self.channels as f32) as f64;
            self.loud_count += 1;
            if self.loud_count == self.loud_window {
                self.flush_loudness();
            }
        }
    }

    fn flush_bucket(&mut self) {
        let q = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0) as i16;
        self.level0.push((q(self.min), q(self.max)));
        self.min = f32::MAX;
        self.max = f32::MIN;
        self.in_bucket = 0;
    }

    fn flush_loudness(&mut self) {
        let rms = (self.loud_acc / self.loud_count as f64).sqrt();
        self.loud.push((20.0 * rms.max(1e-9).log10()) as f32);
        self.loud_acc = 0.0;
        self.loud_count = 0;
    }

    pub fn finish(mut self) -> (Peaks, Loudness) {
        if self.in_bucket > 0 {
            self.flush_bucket();
        }
        if self.loud_count > 0 {
            self.flush_loudness();
        }
        let mut levels = vec![self.level0];
        for _ in 1..LEVELS {
            let next: Vec<(i16, i16)> = levels
                .last()
                .unwrap()
                .chunks(FACTOR as usize)
                .map(|c| {
                    let lo = c.iter().map(|p| p.0).min().unwrap_or(0);
                    let hi = c.iter().map(|p| p.1).max().unwrap_or(0);
                    (lo, hi)
                })
                .collect();
            levels.push(next);
        }
        (Peaks { levels }, Loudness { window: self.loud_window, db: self.loud })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_levels_and_columns() {
        let mut b = AnalysisBuilder::new(1, 44100);
        let samples: Vec<f32> = (0..100_000).map(|i| if i < 50_000 { 0.5 } else { -0.25 }).collect();
        b.push(&samples);
        let (peaks, loud) = b.finish();
        assert_eq!(peaks.levels.len(), LEVELS);
        assert_eq!(peaks.levels[0].len(), 100_000usize.div_ceil(256));
        let cols = peaks.columns(0, 100_000, 2);
        assert!((cols[0].1 - 0.5).abs() < 1e-3);
        assert!((cols[1].0 + 0.25).abs() < 1e-3);
        assert!(!loud.db.is_empty());
    }
}
