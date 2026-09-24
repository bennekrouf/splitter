//! Split suggestions from the loudness track (RMS dBFS per fixed window).

use crate::edit::{DetectParams, Silence, Split};

/// Silence kept at each edge of a cut, so quiet attacks and decays aren't clipped.
pub const CUT_MARGIN_SECS: f64 = 0.25;

/// A run of quiet windows, as window indices `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
}

pub fn silences(db: &[f32], window: u64, rate: u32, p: &DetectParams) -> Vec<Run> {
    let min_windows = ((p.min_silence_secs as f64 * rate as f64) / window as f64).ceil().max(1.0) as usize;
    let mut out = Vec::new();
    let mut run_start = None;
    for (i, &d) in db.iter().chain(std::iter::once(&f32::INFINITY)).enumerate() {
        match (d < p.threshold_db, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(s)) => {
                if i - s >= min_windows {
                    out.push(Run { start: s, end: i });
                }
                run_start = None;
            }
            _ => {}
        }
    }
    out
}

/// Every silence as sample frames, less `CUT_MARGIN_SECS` on each side that borders sound.
/// Lead-in and tail silence reach the recording's start and end (the tail's end may lie past the
/// last sample, since the last window can be partial).
pub fn tag(db: &[f32], window: u64, rate: u32, p: &DetectParams) -> Vec<Silence> {
    let margin = (CUT_MARGIN_SECS * rate as f64) as u64;
    silences(db, window, rate, p)
        .into_iter()
        .filter_map(|r| {
            let start = if r.start == 0 { 0 } else { r.start as u64 * window + margin };
            let end = if r.end >= db.len() { u64::MAX } else { (r.end as u64 * window).saturating_sub(margin) };
            (start < end).then_some(Silence { start, end, keep: false })
        })
        .collect()
}

/// One suggested split in the middle of each silence, skipping lead-in and tail silence, and
/// thinning suggestions closer than `min_track_secs` (the longer silence wins).
pub fn suggest(db: &[f32], window: u64, rate: u32, p: &DetectParams) -> Vec<Split> {
    let secs = |windows: usize| windows as f64 * window as f64 / rate as f64;
    let min_track = (p.min_track_secs as f64 * rate as f64) as u64;
    let mut out: Vec<(Split, usize)> = Vec::new();
    for s in silences(db, window, rate, p) {
        if s.start == 0 || s.end >= db.len() {
            continue; // silence at the very start/end isn't a boundary between tracks
        }
        let len = s.end - s.start;
        let at = ((s.start + s.end) as u64 * window) / 2;
        let split = Split::suggested(at, secs(len) as f32);
        match out.last() {
            Some((prev, prev_len)) if at - prev.at < min_track => {
                if len > *prev_len {
                    *out.last_mut().unwrap() = (split, len);
                }
            }
            _ => out.push((split, len)),
        }
    }
    out.into_iter().map(|(s, _)| s).collect()
}

/// Move `at` to the centre of the quietest window within `radius_secs`. Among equally quiet
/// windows (e.g. digital silence) the one nearest to `at` wins.
pub fn snap(db: &[f32], window: u64, rate: u32, at: u64, radius_secs: f64) -> u64 {
    if db.is_empty() {
        return at;
    }
    let radius = ((radius_secs * rate as f64) / window as f64).ceil() as usize;
    let here = ((at / window) as usize).min(db.len() - 1);
    let lo = here.saturating_sub(radius);
    let hi = (here + radius).min(db.len() - 1);
    let best = (lo..=hi)
        .min_by(|&a, &b| {
            db[a].partial_cmp(&db[b]).unwrap_or(std::cmp::Ordering::Equal).then(a.abs_diff(here).cmp(&b.abs_diff(here)))
        })
        .unwrap_or(here);
    // If `at` is already inside a window as quiet as the best one, don't move it.
    if db[here] <= db[best] {
        return at;
    }
    best as u64 * window + window / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 1000;
    const WIN: u64 = 50; // 20 windows per second

    /// `pattern` of (seconds, dB) segments.
    fn db(pattern: &[(f64, f32)]) -> Vec<f32> {
        pattern.iter().flat_map(|&(s, d)| std::iter::repeat_n(d, (s * 20.0) as usize)).collect()
    }

    fn params() -> DetectParams {
        DetectParams { threshold_db: -45.0, min_silence_secs: 1.5, min_track_secs: 30.0 }
    }

    #[test]
    fn suggests_middle_of_gaps() {
        let d = db(&[(60.0, -20.0), (2.0, -70.0), (60.0, -20.0), (1.0, -70.0), (60.0, -20.0)]);
        let s = suggest(&d, WIN, RATE, &params());
        assert_eq!(s.len(), 1, "the 1 s gap is too short");
        assert_eq!(s[0].at, 61_000);
        assert_eq!(s[0].silence_secs, Some(2.0));
    }

    #[test]
    fn ignores_lead_in_and_tail() {
        let d = db(&[(3.0, -80.0), (60.0, -20.0), (3.0, -80.0)]);
        assert!(suggest(&d, WIN, RATE, &params()).is_empty());
    }

    #[test]
    fn close_suggestions_keep_longest_silence() {
        let d = db(&[(60.0, -20.0), (2.0, -70.0), (10.0, -20.0), (4.0, -70.0), (60.0, -20.0)]);
        let s = suggest(&d, WIN, RATE, &params());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].silence_secs, Some(4.0));
    }

    #[test]
    fn tags_silence_with_margins() {
        let d = db(&[(3.0, -80.0), (60.0, -20.0), (2.0, -70.0), (60.0, -20.0), (3.0, -80.0)]);
        let t = tag(&d, WIN, RATE, &params());
        let bounds: Vec<(u64, u64)> = t.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(bounds, [(0, 2_750), (63_250, 64_750), (125_250, u64::MAX)]);
        // The suggested split sits inside the tagged gap.
        let at = suggest(&d, WIN, RATE, &params())[0].at;
        assert!(t[1].start < at && at < t[1].end);
    }

    #[test]
    fn snap_finds_quiet_spot_nearby() {
        let mut d = vec![-20.0; 200];
        d[110] = -60.0;
        assert_eq!(snap(&d, WIN, RATE, 100 * WIN, 0.5), 110 * WIN + WIN / 2);
        // Out of reach: stays put.
        assert_eq!(snap(&d, WIN, RATE, 50 * WIN, 0.5), 50 * WIN);
    }
}
