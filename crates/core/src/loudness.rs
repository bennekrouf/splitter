//! EBU R128 / ITU-R BS.1770 integrated loudness and true peak of arbitrary ranges, computed
//! from per-block measurements taken once per recording.
//!
//! The analysis stores, every `hop` samples (100 ms), the mean-square energy of the K-weighted
//! 400 ms block ending there, and the true peak within the hop. Integrated loudness of a range
//! is the standard two-stage gated mean over the blocks that lie entirely inside it, so track
//! numbers stay instant however the splits move.

use serde::{Deserialize, Serialize};

/// Absolute gate, LUFS.
const ABSOLUTE_GATE: f64 = -70.0;
/// Relative gate, LU below the absolutely-gated loudness.
const RELATIVE_GATE: f64 = 10.0;
/// Hops per 400 ms block.
const BLOCK_HOPS: usize = 4;
/// ReplayGain 2.0 reference level.
pub const REPLAYGAIN_REFERENCE: f64 = -18.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    /// Integrated loudness in LUFS; `None` if the range is silent or shorter than 400 ms.
    pub lufs: Option<f64>,
    /// True peak in dBTP.
    pub peak_db: f64,
}

fn to_lufs(energy: f64) -> f64 {
    -0.691 + 10.0 * energy.log10()
}

fn to_energy(lufs: f64) -> f64 {
    10f64.powf((lufs + 0.691) / 10.0)
}

/// Two-stage gated mean over block energies.
pub fn integrated(blocks: impl Iterator<Item = f32> + Clone) -> Option<f64> {
    let abs = to_energy(ABSOLUTE_GATE);
    let (sum, n) = blocks.clone().filter(|&e| e as f64 > abs).fold((0.0, 0usize), |(s, n), e| (s + e as f64, n + 1));
    if n == 0 {
        return None;
    }
    let rel = to_energy(to_lufs(sum / n as f64) - RELATIVE_GATE).max(abs);
    let (sum, n) = blocks.filter(|&e| e as f64 > rel).fold((0.0, 0usize), |(s, n), e| (s + e as f64, n + 1));
    (n > 0).then(|| to_lufs(sum / n as f64))
}

/// Block measurements for one recording.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoudnessMap {
    /// Samples per hop (100 ms).
    pub hop: u64,
    /// `block_energy[i]`: K-weighted mean square of the 400 ms block ending at the end of hop `i`.
    pub block_energy: Vec<f32>,
    /// `peak[i]`: true peak (linear, max over channels) within hop `i`.
    pub peak: Vec<f32>,
}

impl LoudnessMap {
    /// Indices of the blocks lying entirely inside `[a, b)`.
    fn blocks_in(&self, a: u64, b: u64) -> std::ops::Range<usize> {
        let first_hop = a.div_ceil(self.hop) as usize;
        let end_hop = ((b / self.hop) as usize).min(self.block_energy.len());
        (first_hop + BLOCK_HOPS - 1).min(end_hop)..end_hop
    }

    fn peak_in(&self, a: u64, b: u64) -> f64 {
        let from = (a / self.hop) as usize;
        let to = (b.div_ceil(self.hop) as usize).min(self.peak.len());
        let p = self.peak.get(from..to).unwrap_or(&[]).iter().fold(0.0f32, |m, &x| m.max(x));
        20.0 * (p.max(1e-10) as f64).log10()
    }

    pub fn range(&self, a: u64, b: u64) -> Loudness {
        self.ranges(&[(a, b)])
    }

    /// Loudness of several ranges taken together (e.g. an album of kept tracks).
    pub fn ranges(&self, ranges: &[(u64, u64)]) -> Loudness {
        if self.hop == 0 {
            return Loudness { lufs: None, peak_db: -200.0 };
        }
        let blocks = ranges.iter().flat_map(|&(a, b)| self.block_energy[self.blocks_in(a, b)].iter().copied());
        let peak_db = ranges.iter().map(|&(a, b)| self.peak_in(a, b)).fold(-200.0, f64::max);
        Loudness { lufs: integrated(blocks), peak_db }
    }
}

/// Gain (dB) that brings `l` to `target_lufs` without the true peak going over `ceiling_db`.
pub fn normalize_gain(l: &Loudness, target_lufs: f64, ceiling_db: f64) -> f64 {
    match l.lufs {
        Some(lufs) => (target_lufs - lufs).min(ceiling_db - l.peak_db),
        None => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(levels: &[(usize, f64)]) -> LoudnessMap {
        // (hops, LUFS) segments; peaks at -3 dBFS everywhere.
        let block_energy: Vec<f32> =
            levels.iter().flat_map(|&(n, l)| std::iter::repeat_n(to_energy(l) as f32, n)).collect();
        let peak = vec![10f32.powf(-3.0 / 20.0); block_energy.len()];
        LoudnessMap { hop: 4410, block_energy, peak }
    }

    #[test]
    fn steady_level_reads_back() {
        let m = map(&[(100, -20.0)]);
        let l = m.range(0, 100 * 4410);
        assert!((l.lufs.unwrap() + 20.0).abs() < 1e-6);
        assert!((l.peak_db + 3.0).abs() < 1e-3);
    }

    #[test]
    fn gates_ignore_silence_and_quiet_passages() {
        // Loud music with silence and a passage 20 LU quieter: both are gated out.
        let m = map(&[(50, -14.0), (30, -120.0), (20, -34.0), (50, -14.0)]);
        let l = m.range(0, 150 * 4410).lufs.unwrap();
        assert!((l + 14.0).abs() < 1e-6, "{l}");
    }

    #[test]
    fn short_or_silent_ranges_have_no_loudness() {
        let m = map(&[(100, -20.0)]);
        assert_eq!(m.range(0, 3 * 4410).lufs, None, "under one 400 ms block");
        assert_eq!(map(&[(100, -90.0)]).range(0, 100 * 4410).lufs, None);
    }

    #[test]
    fn only_blocks_inside_the_range_count() {
        let m = map(&[(40, -30.0), (40, -10.0)]);
        // The second half alone, starting mid-hop.
        let l = m.range(40 * 4410 + 100, 80 * 4410).lufs.unwrap();
        assert!((l + 10.0).abs() < 1e-6, "{l}");
    }

    #[test]
    fn album_loudness_pools_blocks() {
        let m = map(&[(40, -20.0), (40, -20.0)]);
        let l = m.ranges(&[(0, 40 * 4410), (40 * 4410, 80 * 4410)]);
        assert!((l.lufs.unwrap() + 20.0).abs() < 1e-6);
    }

    #[test]
    fn normalization_respects_the_peak_ceiling() {
        let l = Loudness { lufs: Some(-20.0), peak_db: -3.0 };
        assert!((normalize_gain(&l, -14.0, -1.0) - 2.0).abs() < 1e-9, "limited by peak");
        assert!((normalize_gain(&l, -21.0, -1.0) + 1.0).abs() < 1e-9);
    }
}
