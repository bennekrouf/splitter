//! One pass of EBU R128 measurement per recording (400 ms block loudness every 100 ms, and
//! true peak), cached like the scan. Track, album and normalization numbers are then derived
//! instantly by `splitter_core::loudness`.

use crate::export::{ExportJob, ReplayGain};
use crate::scan::{read_cached, write_cached, CacheKey, Scan};
use crate::source::open_source;
use anyhow::{anyhow, Result};
use ebur128::{EbuR128, Mode};
use splitter_core::export::Normalize;
use splitter_core::loudness::{normalize_gain, REPLAYGAIN_REFERENCE};
use std::path::Path;
use std::sync::Arc;

pub use splitter_core::loudness::LoudnessMap;

const MAGIC: &[u8; 8] = b"SPLTLUD1";

pub fn analyze(path: &Path, scan: &Scan, progress: &mut dyn FnMut(f32)) -> Result<LoudnessMap> {
    let mut src = open_source(path, scan.mp3.clone().map(Arc::new))?;
    let ch = src.channels();
    let rate = src.sample_rate();
    let hop = (rate / 10).max(1) as u64;
    let mut ebu =
        EbuR128::new(ch as u32, rate, Mode::M | Mode::TRUE_PEAK).map_err(|e| anyhow!("loudness meter: {e:?}"))?;
    let mut map = LoudnessMap { hop, ..Default::default() };
    let total = scan.info.total_samples.max(1);
    let hop_len = hop as usize * ch;
    let mut pending: Vec<f32> = Vec::with_capacity(hop_len * 2);
    let mut last_report = 0;

    let mut measure = |frames: &[f32], map: &mut LoudnessMap| -> Result<()> {
        ebu.add_frames_f32(frames).map_err(|e| anyhow!("loudness meter: {e:?}"))?;
        let m = ebu.loudness_momentary().unwrap_or(f64::NEG_INFINITY);
        let energy = if m.is_finite() { 10f64.powf((m + 0.691) / 10.0) } else { 0.0 };
        let peak = (0..ch as u32).map(|c| ebu.prev_true_peak(c).unwrap_or(0.0)).fold(0.0, f64::max);
        map.block_energy.push(energy as f32);
        map.peak.push(peak as f32);
        Ok(())
    };

    loop {
        let more = src.read(&mut pending)?;
        let mut at = 0;
        while pending.len() - at >= hop_len {
            measure(&pending[at..at + hop_len], &mut map)?;
            at += hop_len;
        }
        pending.drain(..at);
        if !more {
            if !pending.is_empty() {
                measure(&pending, &mut map)?;
            }
            break;
        }
        let done = map.block_energy.len() as u64 * hop * 100 / total;
        if done > last_report {
            last_report = done;
            progress(done as f32 / 100.0);
        }
    }
    Ok(map)
}

/// Cached analysis if the file is unchanged, otherwise a fresh one (then cached).
pub fn load_or_analyze(path: &Path, scan: &Scan, progress: &mut dyn FnMut(f32)) -> Result<LoudnessMap> {
    let key = CacheKey::of(path)?;
    if let Some(map) = read_cached(path, "loud", MAGIC, &key) {
        return Ok(map);
    }
    let map = analyze(path, scan, progress)?;
    if let Err(e) = write_cached(path, "loud", MAGIC, &key, &map) {
        eprintln!("splitter: could not cache loudness for {}: {e:#}", path.display());
    }
    Ok(map)
}

/// Set each job's normalization gain (only when `reencode`) and ReplayGain tags describing
/// the audio as written, i.e. after that gain.
pub fn apply_to_jobs(jobs: &mut [ExportJob], map: &LoudnessMap, normalize: Normalize, reencode: bool) {
    let ranges: Vec<(u64, u64)> = jobs.iter().map(|j| (j.start, j.end)).collect();
    let album = map.ranges(&ranges);
    let ceiling = Normalize::CEILING_DB;
    let album_gain = match normalize {
        Normalize::Album { target } if reencode => normalize_gain(&album, target as f64, ceiling),
        _ => 0.0,
    };
    for job in jobs.iter_mut() {
        let track = map.range(job.start, job.end);
        job.gain_db = match normalize {
            _ if !reencode => 0.0,
            Normalize::Off => 0.0,
            Normalize::Album { .. } => album_gain,
            Normalize::Track { target } => normalize_gain(&track, target as f64, ceiling),
        };
        let Some(lufs) = track.lufs else {
            job.tags.replaygain = None;
            continue;
        };
        let g = job.gain_db;
        // With per-track gains the album figures no longer describe one consistent level.
        let album_rg = match (album.lufs, normalize) {
            (Some(a), Normalize::Off | Normalize::Album { .. }) => {
                Some((REPLAYGAIN_REFERENCE - (a + g), 10f64.powf((album.peak_db + g) / 20.0)))
            }
            _ => None,
        };
        job.tags.replaygain = Some(ReplayGain {
            track_gain_db: REPLAYGAIN_REFERENCE - (lufs + g),
            track_peak: 10f64.powf((track.peak_db + g) / 20.0),
            album: album_rg,
        });
    }
}
