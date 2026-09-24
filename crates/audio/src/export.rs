//! Lossless export of track ranges.
//!
//! **MP3**: the original frames are copied, never re-encoded. A track starting in frame `k`
//! gets frames `k-2..` (so the MDCT overlap and filterbank are primed), preceded by one silent
//! *carrier* frame whose main data holds the 511 bytes of bit reservoir those frames may read
//! from. That bounds the warm-up to under four frames at any bitrate, which the Info/LAME
//! header's 12-bit encoder delay can always describe, so gapless decoders (ffmpeg, symphonia,
//! iTunes, foobar…) trim the output to exactly `[start, end)`. Players that ignore the LAME
//! header play up to ~0.1 s extra at the edges.
//!
//! **WAV**: the sample bytes are copied with the original `fmt ` chunk, so it is bit-exact.

use crate::mp3index::{parse_header, Mp3Index};
use crate::scan::{Bitrate, Scan};
use crate::source::open_source;
use crate::transcode::encode_track;
use anyhow::{anyhow, bail, Context, Result};
use splitter_core::export::Profile;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Samples every MP3 decoder outputs before the first real sample (LAME convention).
const DECODER_DELAY: u64 = 529;
/// The LAME header stores delay and padding in 12 bits each.
const MAX_LAME_FIELD: u64 = 4095;
/// Largest possible `main_data_begin`: how far back a frame can read the bit reservoir.
const MAX_RESERVOIR: usize = 511;
/// Frames copied before the one containing the track start.
const PRIME_FRAMES: usize = 2;

#[derive(Clone, Debug, Default)]
pub struct Tags {
    pub title: String,
    pub album: String,
    pub track: usize,
    pub total: usize,
    pub replaygain: Option<ReplayGain>,
}

/// ReplayGain 2.0 values (reference −18 LUFS), as written to tags.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReplayGain {
    pub track_gain_db: f64,
    /// Linear true peak.
    pub track_peak: f64,
    /// (gain dB, linear peak) for the whole set of exported tracks.
    pub album: Option<(f64, f64)>,
}

impl ReplayGain {
    /// `(key, value)` pairs in the usual text form.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut f = vec![
            ("REPLAYGAIN_TRACK_GAIN", format!("{:+.2} dB", self.track_gain_db)),
            ("REPLAYGAIN_TRACK_PEAK", format!("{:.6}", self.track_peak)),
        ];
        if let Some((gain, peak)) = self.album {
            f.push(("REPLAYGAIN_ALBUM_GAIN", format!("{gain:+.2} dB")));
            f.push(("REPLAYGAIN_ALBUM_PEAK", format!("{peak:.6}")));
        }
        f
    }
}

#[derive(Clone, Debug)]
pub struct ExportJob {
    pub start: u64,
    pub end: u64,
    pub path: PathBuf,
    pub tags: Tags,
    /// Level change in dB, applied only when re-encoding.
    pub gain_db: f64,
}

/// Write every job with `profile`. `progress` is called with the number of files finished.
pub fn export(
    source: &Path,
    scan: &Scan,
    jobs: &[ExportJob],
    profile: Profile,
    progress: &mut dyn FnMut(usize),
) -> Result<()> {
    let mut src = File::open(source).with_context(|| format!("opening {}", source.display()))?;
    let wav = match (&scan.mp3, profile) {
        (None, Profile::Original) => Some(WavLayout::read(&mut src).context("reading the WAV structure")?),
        _ => None,
    };
    let mut pcm = match profile {
        Profile::Original => None,
        _ => Some(open_source(source, scan.mp3.clone().map(Arc::new))?),
    };
    let bits = match scan.info.bitrate {
        Bitrate::Pcm { bits, .. } => Some(bits),
        _ => None,
    };
    for (i, job) in jobs.iter().enumerate() {
        if let Some(dir) = job.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write to a temporary name, so a failed export never leaves a half-written track.
        let tmp = job.path.with_extension("part");
        let result = match (&mut pcm, &scan.mp3, &wav) {
            (Some(pcm), _, _) => (|| {
                let mut w = BufWriter::new(File::create(&tmp)?);
                encode_track(profile, pcm.as_mut(), job.start, job.end, bits, job.gain_db, &job.tags, &mut w)?;
                w.flush()?;
                Ok(())
            })(),
            (None, Some(index), _) => write_mp3(&mut src, index, job, &tmp),
            (None, None, Some(layout)) => write_wav(&mut src, layout, job, &tmp),
            _ => unreachable!(),
        };
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.context(format!("writing {}", job.path.display())));
        }
        std::fs::rename(&tmp, &job.path)?;
        progress(i + 1);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// MP3

/// Which frames to copy and the LAME trim values for one track.
#[derive(Debug, PartialEq)]
struct Mp3Cut {
    first_frame: usize,
    /// Source frames copied.
    frames: usize,
    /// Whether a reservoir carrier frame goes in front.
    carrier: bool,
    enc_delay: u64,
    enc_padding: u64,
}

fn plan_mp3_cut(index: &Mp3Index, start: u64, end: u64) -> Result<Mp3Cut> {
    let spf = index.samples_per_frame as u64;
    let n = index.frames();
    if end <= start || start >= index.total_samples() {
        bail!("empty track");
    }
    let first = (start / spf) as usize - ((start / spf) as usize).min(PRIME_FRAMES);
    let carrier = first > 0;
    // Output sample 0 is the carrier's (silent) first sample, if there is one.
    let lead = first as u64 * spf - if carrier { spf } else { 0 };
    let skip = start - lead;
    // Near the very start of a file the decoder delay can't be hidden; the track then starts
    // up to 529 samples (12 ms) late, inside the encoder's own lead-in silence.
    let enc_delay = skip.saturating_sub(DECODER_DELAY);
    debug_assert!(enc_delay <= MAX_LAME_FIELD);
    let actual_start = lead + enc_delay + DECODER_DELAY;
    let last = (end.div_ceil(spf) as usize).min(n);
    let frames = last - first;
    let decoded = (frames as u64 + carrier as u64) * spf;
    let keep = end.saturating_sub(actual_start);
    let enc_padding = decoded.saturating_sub(enc_delay + keep).min(MAX_LAME_FIELD);
    Ok(Mp3Cut { first_frame: first, frames, carrier, enc_delay, enc_padding })
}

fn write_mp3(src: &mut File, index: &Mp3Index, job: &ExportJob, out: &Path) -> Result<()> {
    let cut = plan_mp3_cut(index, job.start, job.end)?;
    let from = index.offsets[cut.first_frame];
    let to = index.frame_end(cut.first_frame + cut.frames - 1);
    let mut frames = vec![0u8; (to - from) as usize];
    src.seek(SeekFrom::Start(from))?;
    src.read_exact(&mut frames)?;

    let mut first_header = [0u8; 4];
    first_header.copy_from_slice(&frames[..4]);
    let mut offsets: Vec<u64> =
        index.offsets[cut.first_frame..cut.first_frame + cut.frames].iter().map(|o| o - from).collect();
    let audio = if cut.carrier {
        let reservoir = index.reservoir_before(src, cut.first_frame, MAX_RESERVOIR)?;
        let carrier = carrier_frame(first_header, &reservoir)?;
        offsets.iter_mut().for_each(|o| *o += carrier.len() as u64);
        offsets.insert(0, 0);
        [carrier, frames].concat()
    } else {
        frames
    };
    let info = info_frame(first_header, index.is_vbr(), &offsets, &audio, cut.enc_delay, cut.enc_padding)?;

    let mut w = BufWriter::new(File::create(out)?);
    w.write_all(&id3v2(&job.tags))?;
    w.write_all(&info)?;
    w.write_all(&audio)?;
    w.flush()?;
    Ok(())
}

/// `template` with no CRC, no padding bit, and the smallest bitrate whose frame has at least
/// `main_data` bytes after the side information. Returns (header, frame length, side info length).
fn roomy_header(template: [u8; 4], main_data: usize) -> Result<([u8; 4], usize, usize)> {
    let mut h = template;
    h[1] |= 0x01;
    h[2] &= !0x02;
    let side = parse_header(&h).ok_or_else(|| anyhow!("unexpected MP3 header"))?.side_info_len();
    (1..15u8)
        .find_map(|idx| {
            let mut t = h;
            t[2] = (t[2] & 0x0F) | (idx << 4);
            let p = parse_header(&t)?;
            (p.frame_len >= 4 + side + main_data).then_some((t, p.frame_len, side))
        })
        .ok_or_else(|| anyhow!("no MP3 bitrate is large enough"))
}

/// A frame that decodes to silence (all-zero side info) and whose main data ends with
/// `reservoir`, the bytes the following frames reach back for.
fn carrier_frame(template: [u8; 4], reservoir: &[u8]) -> Result<Vec<u8>> {
    let (h, len, _) = roomy_header(template, reservoir.len())?;
    let mut f = vec![0u8; len];
    f[..4].copy_from_slice(&h);
    f[len - reservoir.len()..].copy_from_slice(reservoir);
    Ok(f)
}

/// An Info (CBR) / Xing (VBR) frame with a LAME extension carrying the gapless trim.
fn info_frame(
    template: [u8; 4],
    vbr: bool,
    frame_offsets: &[u64],
    audio: &[u8],
    enc_delay: u64,
    enc_padding: u64,
) -> Result<Vec<u8>> {
    // Same version / rate / channel mode as the audio; the tag lives after the side info.
    let (h, frame_len, side) = roomy_header(template, 120 + 36)?;
    let tag_start = 4 + side;

    let mut f = vec![0u8; frame_len];
    f[..4].copy_from_slice(&h);
    let total_bytes = (frame_len + audio.len()) as u32;

    let mut t: Vec<u8> = Vec::with_capacity(156);
    t.extend_from_slice(if vbr { b"Xing" } else { b"Info" });
    t.extend_from_slice(&0x0Fu32.to_be_bytes()); // frames, bytes, TOC, quality
    t.extend_from_slice(&(frame_offsets.len() as u32).to_be_bytes());
    t.extend_from_slice(&total_bytes.to_be_bytes());
    // TOC: byte position (in 1/256ths of the file) at each percent of the frames.
    for i in 0..100 {
        let frame = (i * frame_offsets.len() / 100).min(frame_offsets.len() - 1);
        let pos = frame_len as u64 + frame_offsets[frame];
        t.push((pos * 256 / total_bytes as u64).min(255) as u8);
    }
    t.extend_from_slice(&0u32.to_be_bytes()); // quality

    // LAME extension (36 bytes).
    t.extend_from_slice(b"LAME3.100");
    t.push(0x00); // tag revision 0, VBR method unknown
    t.push(0x00); // lowpass
    t.extend_from_slice(&0u32.to_be_bytes()); // peak
    t.extend_from_slice(&0u16.to_be_bytes()); // radio gain
    t.extend_from_slice(&0u16.to_be_bytes()); // audiophile gain
    t.push(0x00); // encoding flags / ATH
    t.push(0x00); // bitrate
    let trim = ((enc_delay.min(MAX_LAME_FIELD) as u32) << 12) | enc_padding.min(MAX_LAME_FIELD) as u32;
    t.extend_from_slice(&trim.to_be_bytes()[1..]);
    t.push(0x00); // misc
    t.push(0x00); // mp3 gain
    t.extend_from_slice(&0u16.to_be_bytes()); // preset / surround
    t.extend_from_slice(&total_bytes.to_be_bytes()); // music length
    t.extend_from_slice(&crc16(audio).to_be_bytes()); // music CRC
    f[tag_start..tag_start + t.len()].copy_from_slice(&t);
    // Tag CRC over every byte of the frame before it.
    let crc_at = tag_start + t.len();
    let crc = crc16(&f[..crc_at]);
    f[crc_at..crc_at + 2].copy_from_slice(&crc.to_be_bytes());
    Ok(f)
}

/// CRC-16/ARC (reflected 0x8005, init 0), as used by the LAME tag.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

/// Minimal ID3v2.3 tag: title, album and track number.
pub(crate) fn id3v2(tags: &Tags) -> Vec<u8> {
    let mut frames = Vec::new();
    let mut text = |id: &[u8; 4], value: &str| {
        if value.is_empty() {
            return;
        }
        // UTF-16 with BOM, so any title survives.
        let mut body = vec![0x01, 0xFF, 0xFE];
        for u in value.encode_utf16() {
            body.extend_from_slice(&u.to_le_bytes());
        }
        frames.extend_from_slice(id);
        frames.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend_from_slice(&body);
    };
    text(b"TIT2", &tags.title);
    text(b"TALB", &tags.album);
    if tags.track > 0 {
        text(b"TRCK", &format!("{}/{}", tags.track, tags.total));
    }
    // ReplayGain as user-defined text frames (Latin-1: description, NUL, value).
    for (key, value) in tags.replaygain.iter().flat_map(|rg| rg.fields()) {
        let mut body = vec![0x00];
        body.extend_from_slice(key.as_bytes());
        body.push(0);
        body.extend_from_slice(value.as_bytes());
        frames.extend_from_slice(b"TXXX");
        frames.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend_from_slice(&body);
    }
    let size = frames.len() as u32;
    let mut out = b"ID3\x03\x00\x00".to_vec();
    out.extend((0..4).rev().map(|i| ((size >> (7 * i)) & 0x7F) as u8)); // synchsafe
    out.extend(frames);
    out
}

// ---------------------------------------------------------------------------------------------
// WAV

#[derive(Debug)]
pub(crate) struct WavLayout {
    pub(crate) fmt: Vec<u8>,
    pub(crate) data_offset: u64,
    pub(crate) data_len: u64,
    pub(crate) block_align: u64,
}

impl WavLayout {
    pub(crate) fn read(f: &mut File) -> Result<Self> {
        let mut head = [0u8; 12];
        f.seek(SeekFrom::Start(0))?;
        f.read_exact(&mut head)?;
        if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
            bail!("not a RIFF/WAVE file (RF64 and others aren't supported yet)");
        }
        let file_len = f.metadata()?.len();
        let mut pos = 12u64;
        let mut fmt = None;
        while pos + 8 <= file_len {
            let mut ch = [0u8; 8];
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut ch)?;
            let size = u32::from_le_bytes(ch[4..8].try_into().unwrap()) as u64;
            let body = pos + 8;
            match &ch[0..4] {
                b"fmt " => {
                    let mut buf = vec![0u8; size as usize];
                    f.read_exact(&mut buf)?;
                    fmt = Some(buf);
                }
                b"data" => {
                    let fmt = fmt.ok_or_else(|| anyhow!("data chunk before fmt chunk"))?;
                    let block_align = u16::from_le_bytes([fmt[12], fmt[13]]) as u64;
                    if block_align == 0 {
                        bail!("invalid block alignment");
                    }
                    let data_len = size.min(file_len - body);
                    return Ok(Self { fmt, data_offset: body, data_len, block_align });
                }
                _ => {}
            }
            pos = body + size + (size & 1);
        }
        bail!("no data chunk")
    }
}

fn write_wav(src: &mut File, layout: &WavLayout, job: &ExportJob, out: &Path) -> Result<()> {
    let frames_total = layout.data_len / layout.block_align;
    let (a, b) = (job.start.min(frames_total), job.end.min(frames_total));
    if b <= a {
        bail!("empty track");
    }
    let len = (b - a) * layout.block_align;
    let fmt_len = layout.fmt.len() as u64;
    let riff_len = 4 + (8 + fmt_len + (fmt_len & 1)) + (8 + len + (len & 1));
    if riff_len > u32::MAX as u64 {
        bail!("track is larger than 4 GB, which WAV can't hold");
    }

    let mut w = BufWriter::new(File::create(out)?);
    w.write_all(b"RIFF")?;
    w.write_all(&(riff_len as u32).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&(fmt_len as u32).to_le_bytes())?;
    w.write_all(&layout.fmt)?;
    if fmt_len & 1 == 1 {
        w.write_all(&[0])?;
    }
    w.write_all(b"data")?;
    w.write_all(&(len as u32).to_le_bytes())?;
    src.seek(SeekFrom::Start(layout.data_offset + a * layout.block_align))?;
    std::io::copy(&mut src.take(len), &mut w)?;
    if len & 1 == 1 {
        w.write_all(&[0])?;
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_matches_reference() {
        // CRC-16/ARC check value.
        assert_eq!(crc16(b"123456789"), 0xBB3D);
    }

    #[test]
    fn id3_is_well_formed() {
        let tag = id3v2(&Tags { title: "Été".into(), album: "Live".into(), track: 3, total: 12, replaygain: None });
        assert_eq!(&tag[..3], b"ID3");
        let size = tag[6..10].iter().fold(0usize, |a, &b| (a << 7) | b as usize);
        assert_eq!(size + 10, tag.len());
    }
}
