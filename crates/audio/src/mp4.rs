//! Just enough of the MP4/QuickTime box structure to line our timeline up with everyone
//! else's.
//!
//! Symphonia decodes every AAC packet from the first, including the encoder's priming samples
//! (typically 1024 or 2112). The file's edit list says to skip them, and ffmpeg and the web
//! views do. So our sample frame `f` is at `f / rate - audio_delay` seconds in the video.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Seconds of priming at the start of the audio track that players skip, from its edit list.
/// `None` if the file has no audio track or edit list saying so (then nothing is skipped).
pub fn audio_delay_secs(path: &Path) -> Option<f64> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let moov = find_box(&mut f, 0, len, b"moov")?;
    let mut buf = vec![0; usize::try_from(moov.1 - moov.0).ok()?];
    f.seek(SeekFrom::Start(moov.0)).ok()?;
    f.read_exact(&mut buf).ok()?;
    let delay = children(&buf).filter(|(t, _)| t == b"trak").find_map(|(_, trak)| audio_delay_of_trak(trak));
    delay
}

fn audio_delay_of_trak(trak: &[u8]) -> Option<f64> {
    let mdia = child(trak, b"mdia")?;
    // hdlr: version/flags (4), pre_defined (4), handler type (4).
    if child(mdia, b"hdlr")?.get(8..12)? != b"soun" {
        return None;
    }
    let mdhd = child(mdia, b"mdhd")?;
    let timescale = match mdhd.first()? {
        0 => u32::from_be_bytes(mdhd.get(12..16)?.try_into().ok()?),
        _ => u32::from_be_bytes(mdhd.get(20..24)?.try_into().ok()?),
    };
    let elst = child(child(trak, b"edts")?, b"elst")?;
    let (version, count) = (*elst.first()?, u32::from_be_bytes(elst.get(4..8)?.try_into().ok()?));
    let size = if version == 1 { 20 } else { 12 };
    // The first edit that shows media (an empty edit, media_time -1, only delays the track).
    (0..count as usize).find_map(|i| {
        let e = elst.get(8 + i * size..8 + (i + 1) * size)?;
        let media_time = match version {
            1 => i64::from_be_bytes(e[8..16].try_into().ok()?),
            _ => i32::from_be_bytes(e[4..8].try_into().ok()?) as i64,
        };
        (media_time >= 0 && timescale > 0).then(|| media_time as f64 / timescale as f64)
    })
}

/// A box header at `pos`: (type, start of its body, end of the box).
fn header(f: &mut File, pos: u64, end: u64) -> Option<([u8; 4], u64, u64)> {
    let mut h = [0u8; 16];
    f.seek(SeekFrom::Start(pos)).ok()?;
    f.read_exact(&mut h[..8]).ok()?;
    let size = u32::from_be_bytes(h[..4].try_into().ok()?) as u64;
    let kind: [u8; 4] = h[4..8].try_into().ok()?;
    let (body, size) = match size {
        0 => (pos + 8, end - pos),
        1 => {
            f.read_exact(&mut h[8..16]).ok()?;
            (pos + 16, u64::from_be_bytes(h[8..16].try_into().ok()?))
        }
        s => (pos + 8, s),
    };
    (size >= body - pos && pos + size <= end).then_some((kind, body, pos + size))
}

/// The body `(start, end)` of the first top-level box of type `kind`, without reading the
/// others (the media data can be gigabytes, and moov can come after it).
fn find_box(f: &mut File, mut pos: u64, end: u64, kind: &[u8; 4]) -> Option<(u64, u64)> {
    while pos + 8 <= end {
        let (t, body, next) = header(f, pos, end)?;
        if &t == kind {
            return Some((body, next));
        }
        pos = next;
    }
    None
}

/// The boxes inside a box body held in memory.
fn children(mut data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let size = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
        let kind: [u8; 4] = data.get(4..8)?.try_into().ok()?;
        let (start, size) = match size {
            0 => (8, data.len()),
            1 => (16, usize::try_from(u64::from_be_bytes(data.get(8..16)?.try_into().ok()?)).ok()?),
            s => (8, s),
        };
        let body = data.get(start..size)?;
        data = &data[size..];
        Some((kind, body))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(data).find(|(t, _)| t == kind).map(|(_, b)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_aac_priming_from_the_edit_list() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video.mp4");
        // ffmpeg's AAC encoder primes with 1024 samples; the video track's own edit (B-frame
        // delay) must not be taken for it.
        assert_eq!(audio_delay_secs(&path), Some(1024.0 / 44100.0));
    }

    #[test]
    fn nothing_for_files_without_one() {
        let path = std::env::temp_dir().join("splitter-mp4-test.mp4");
        std::fs::write(&path, b"not an mp4 at all").unwrap();
        assert_eq!(audio_delay_secs(&path), None);
    }
}
