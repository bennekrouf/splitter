//! One pass over a recording: frame index (MP3), peaks, loudness and source info.
//! The result is cached in the user cache dir, keyed by path, size and mtime.

use crate::mp3index::Mp3Index;
use crate::peaks::{AnalysisBuilder, Loudness, Peaks};
use crate::source::Decoding;
use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Bitrate {
    Cbr(u32),
    Vbr { min: u32, avg: u32, max: u32 },
    Pcm { bits: u32, kbps: u32 },
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceInfo {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Playable length in sample frames, as decoded.
    pub total_samples: u64,
    pub file_size: u64,
    pub bitrate: Bitrate,
}

impl SourceInfo {
    pub fn duration_secs(&self) -> f64 {
        self.total_samples as f64 / self.sample_rate as f64
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scan {
    pub info: SourceInfo,
    pub mp3: Option<Mp3Index>,
    pub peaks: Peaks,
    pub loudness: Loudness,
}

pub fn scan(path: &Path, progress: &mut dyn FnMut(f32)) -> Result<Scan> {
    let file_size = std::fs::metadata(path)?.len();
    let is_mp3 = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3"));
    let mp3 = if is_mp3 { Some(Mp3Index::build(path)?) } else { None };
    progress(0.02);

    let mut dec = Decoding::probe(path)?;
    let track = dec.format().default_track().ok_or_else(|| anyhow!("no audio track"))?;
    let params = track.codec_params.clone();
    let sample_rate = params.sample_rate.ok_or_else(|| anyhow!("unknown sample rate"))?;
    let channels = params.channels.map(|c| c.count()).unwrap_or(1);
    let expected = mp3.as_ref().map(|m| m.total_samples()).or(params.n_frames).unwrap_or(0);

    let mut analysis = AnalysisBuilder::new(channels, sample_rate);
    let mut buf = Vec::with_capacity(8192);
    let mut total: u64 = 0;
    let mut last_report = 0.0;
    loop {
        buf.clear();
        match dec.read(&mut buf) {
            Ok(true) => {}
            Ok(false) => break,
            // A truncated or damaged tail shouldn't make the whole recording unusable.
            Err(_) if total > 0 => break,
            Err(e) => return Err(e),
        }
        analysis.push(&buf);
        total += (buf.len() / channels) as u64;
        if expected > 0 {
            let p = 0.02 + 0.98 * (total as f32 / expected as f32).min(1.0);
            if p - last_report >= 0.01 {
                last_report = p;
                progress(p);
            }
        }
    }
    let (peaks, loudness) = analysis.finish();

    let (codec, bitrate) = match &mp3 {
        Some(m) => (
            "MP3".to_string(),
            if m.is_vbr() {
                Bitrate::Vbr { min: m.min_kbps, avg: m.avg_kbps, max: m.max_kbps }
            } else {
                Bitrate::Cbr(m.avg_kbps)
            },
        ),
        None => match params.bits_per_sample {
            Some(bits) => (
                format!("PCM {bits}-bit"),
                Bitrate::Pcm { bits, kbps: sample_rate * channels as u32 * bits / 1000 },
            ),
            None => ("PCM".to_string(), Bitrate::Unknown),
        },
    };

    Ok(Scan {
        info: SourceInfo {
            codec,
            sample_rate,
            channels: channels as u16,
            total_samples: total,
            file_size,
            bitrate,
        },
        mp3,
        peaks,
        loudness,
    })
}

const SCAN_MAGIC: &[u8; 8] = b"SPLTIDX1";

/// Identifies the exact file a cache entry was computed from.
#[derive(PartialEq, Serialize, Deserialize)]
pub(crate) struct CacheKey {
    path: PathBuf,
    size: u64,
    mtime_ns: u128,
}

impl CacheKey {
    pub(crate) fn of(path: &Path) -> Result<Self> {
        let meta = std::fs::metadata(path)?;
        let mtime_ns = meta.modified()?.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        Ok(Self { path: path.to_owned(), size: meta.len(), mtime_ns })
    }
}

fn cache_file(path: &Path, ext: &str) -> Option<PathBuf> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    Some(dirs::cache_dir()?.join("splitter").join(format!("{:016x}.{ext}", h.finish())))
}

/// A cached `T` for `path`, if one exists for this exact version of the file.
pub(crate) fn read_cached<T: DeserializeOwned>(path: &Path, ext: &str, magic: &[u8; 8], key: &CacheKey) -> Option<T> {
    let mut r = BufReader::new(File::open(cache_file(path, ext)?).ok()?);
    let mut m = [0u8; 8];
    std::io::Read::read_exact(&mut r, &mut m).ok()?;
    if &m != magic {
        return None;
    }
    let stored: CacheKey = bincode::deserialize_from(&mut r).ok()?;
    if &stored != key {
        return None;
    }
    bincode::deserialize_from(&mut r).ok()
}

pub(crate) fn write_cached<T: Serialize>(path: &Path, ext: &str, magic: &[u8; 8], key: &CacheKey, value: &T) -> Result<()> {
    let file = cache_file(path, ext).ok_or_else(|| anyhow!("no cache dir"))?;
    std::fs::create_dir_all(file.parent().unwrap())?;
    let tmp = file.with_extension(format!("{ext}.tmp"));
    let mut w = BufWriter::new(File::create(&tmp)?);
    w.write_all(magic)?;
    bincode::serialize_into(&mut w, key)?;
    bincode::serialize_into(&mut w, value)?;
    w.flush()?;
    drop(w);
    std::fs::rename(tmp, file)?;
    Ok(())
}

/// Cached scan if the file is unchanged, otherwise a fresh scan (then cached).
pub fn load_or_scan(path: &Path, progress: &mut dyn FnMut(f32)) -> Result<Scan> {
    let key = CacheKey::of(path)?;
    if let Some(scan) = read_cached(path, "idx", SCAN_MAGIC, &key) {
        return Ok(scan);
    }
    let scan = scan(path, progress)?;
    if let Err(e) = write_cached(path, "idx", SCAN_MAGIC, &key, &scan) {
        eprintln!("splitter: could not cache scan for {}: {e:#}", path.display());
    }
    Ok(scan)
}
