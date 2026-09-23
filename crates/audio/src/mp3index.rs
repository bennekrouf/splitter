//! Byte-offset index of every MPEG Layer III frame in a file.
//!
//! Frame `i` holds samples `[i * spf, (i + 1) * spf)` in the same numbering symphonia uses
//! with gapless mode off (Xing/Info/VBRI tag frames are skipped, exactly like `MpaReader`).
//! This gives exact seeking on VBR files and is what lossless frame-copy export will use.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MpegVersion {
    V1,
    V2,
    V2_5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub version: MpegVersion,
    pub sample_rate: u32,
    pub bitrate_kbps: u32,
    pub channels: u16,
    pub frame_len: usize,
    pub samples: u32,
}

impl FrameHeader {
    pub(crate) fn side_info_len(&self) -> usize {
        match (self.version, self.channels) {
            (MpegVersion::V1, 1) => 17,
            (MpegVersion::V1, _) => 32,
            (_, 1) => 9,
            _ => 17,
        }
    }

    fn compatible(&self, other: &FrameHeader) -> bool {
        self.version == other.version && self.sample_rate == other.sample_rate
    }
}

const BITRATES_V1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const BITRATES_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// Parse a Layer III frame header. Free-format and reserved values are rejected.
pub fn parse_header(b: &[u8]) -> Option<FrameHeader> {
    if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = match (b[1] >> 3) & 0b11 {
        0b00 => MpegVersion::V2_5,
        0b10 => MpegVersion::V2,
        0b11 => MpegVersion::V1,
        _ => return None,
    };
    if (b[1] >> 1) & 0b11 != 0b01 {
        return None; // not Layer III
    }
    let br_idx = (b[2] >> 4) as usize;
    let sr_idx = ((b[2] >> 2) & 0b11) as usize;
    if br_idx == 0 || br_idx == 15 || sr_idx == 3 {
        return None;
    }
    let padding = ((b[2] >> 1) & 1) as usize;
    let channels = if (b[3] >> 6) == 0b11 { 1 } else { 2 };
    let (bitrate_kbps, sample_rate, samples) = match version {
        MpegVersion::V1 => (BITRATES_V1[br_idx], [44100, 48000, 32000][sr_idx], 1152),
        MpegVersion::V2 => (BITRATES_V2[br_idx], [22050, 24000, 16000][sr_idx], 576),
        MpegVersion::V2_5 => (BITRATES_V2[br_idx], [11025, 12000, 8000][sr_idx], 576),
    };
    let frame_len = (samples as usize / 8) * bitrate_kbps as usize * 1000 / sample_rate as usize + padding;
    Some(FrameHeader { version, sample_rate, bitrate_kbps, channels, frame_len, samples })
}

/// Xing/Info or VBRI tag frames carry no audio; symphonia drops them, so must we.
fn is_tag_frame(frame: &[u8], h: &FrameHeader) -> bool {
    let off = 4 + h.side_info_len();
    if frame.len() >= off + 4 {
        let id = &frame[off..off + 4];
        if (id == b"Xing" || id == b"Info") && frame[4..off].iter().all(|&b| b == 0) {
            return true;
        }
    }
    frame.len() >= 40 && &frame[36..40] == b"VBRI"
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mp3Index {
    /// Byte offset of each audio frame.
    pub offsets: Vec<u64>,
    pub samples_per_frame: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub min_kbps: u32,
    pub max_kbps: u32,
    pub avg_kbps: u32,
    /// Byte offset just past the last audio frame (start of trailing tags, if any).
    pub audio_end: u64,
}

impl Mp3Index {
    pub fn frames(&self) -> usize {
        self.offsets.len()
    }

    pub fn total_samples(&self) -> u64 {
        self.offsets.len() as u64 * self.samples_per_frame as u64
    }

    pub fn is_vbr(&self) -> bool {
        self.min_kbps != self.max_kbps
    }

    /// Byte offset just past frame `i`.
    pub fn frame_end(&self, i: usize) -> u64 {
        self.offsets.get(i + 1).copied().unwrap_or(self.audio_end)
    }

    /// Earliest frame to feed a decoder so that frame `k` comes out exactly as in a full decode.
    ///
    /// Frame `k` depends on the two frames before it (MDCT overlap, and the synthesis
    /// filterbank state for single-granule MPEG-2 frames), and each of those three frames may
    /// take its audio data from up to 511 bytes back — the bit reservoir — which at low
    /// bitrates spans several frames.
    pub fn decode_start(&self, file: &mut File, k: usize) -> std::io::Result<usize> {
        let k = k.min(self.frames().saturating_sub(1));
        let mut start = k.saturating_sub(2);
        for j in k.saturating_sub(2)..=k {
            start = start.min(self.reservoir_start(file, j)?);
        }
        Ok(start)
    }

    /// The earliest frame whose audio data frame `j` reads from.
    fn reservoir_start(&self, file: &mut File, j: usize) -> std::io::Result<usize> {
        let (mut back, _) = self.side_info(file, j)?;
        let mut i = j;
        while back > 0 && i > 0 {
            i -= 1;
            let (_, len) = self.side_info(file, i)?;
            if len >= back {
                break;
            }
            back -= len;
        }
        Ok(i)
    }

    /// The last `n` bytes of the main-data stream (the bit reservoir) before frame `first`,
    /// front-padded with zeros if the stream is shorter.
    pub fn reservoir_before(&self, file: &mut File, first: usize, n: usize) -> std::io::Result<Vec<u8>> {
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        let mut have = 0;
        let mut i = first;
        while have < n && i > 0 {
            i -= 1;
            let mut b = [0u8; 4];
            file.seek(SeekFrom::Start(self.offsets[i]))?;
            file.read_exact(&mut b)?;
            let h = parse_header(&b).ok_or_else(|| std::io::Error::other("bad frame header in index"))?;
            let head = 4 + if b[1] & 1 == 0 { 2 } else { 0 } + h.side_info_len() as u64;
            let (from, to) = (self.offsets[i] + head, self.frame_end(i));
            let mut data = vec![0u8; to.saturating_sub(from) as usize];
            file.seek(SeekFrom::Start(from))?;
            file.read_exact(&mut data)?;
            have += data.len();
            chunks.push(data);
        }
        let stream: Vec<u8> = chunks.into_iter().rev().flatten().collect();
        let mut out = vec![0u8; n.saturating_sub(stream.len())];
        out.extend_from_slice(&stream[stream.len().saturating_sub(n)..]);
        Ok(out)
    }

    /// (main_data_begin, main data bytes) of frame `i`.
    fn side_info(&self, file: &mut File, i: usize) -> std::io::Result<(usize, usize)> {
        let mut b = [0u8; 8];
        file.seek(SeekFrom::Start(self.offsets[i]))?;
        file.read_exact(&mut b)?;
        let h = parse_header(&b).ok_or_else(|| std::io::Error::other("bad frame header in index"))?;
        let crc = if b[1] & 1 == 0 { 2 } else { 0 };
        let s = 4 + crc;
        let begin = match h.version {
            MpegVersion::V1 => ((b[s] as usize) << 1) | (b[s + 1] as usize >> 7),
            _ => b[s] as usize,
        };
        let frame_len = (self.frame_end(i) - self.offsets[i]) as usize;
        Ok((begin, frame_len.saturating_sub(4 + crc + h.side_info_len())))
    }

    pub fn build(path: &Path) -> Result<Self> {
        let mut r = Window::open(path)?;
        let file_len = r.len;
        let mut pos = id3v2_len(&mut r);

        let mut offsets = Vec::new();
        let mut first: Option<FrameHeader> = None;
        let (mut min_kbps, mut max_kbps, mut sum_kbps) = (u32::MAX, 0u32, 0u64);
        let mut audio_end = pos;

        while pos + 4 <= file_len {
            let h = match r.get(pos, 4).and_then(parse_header) {
                Some(h) if first.is_none_or(|f| f.compatible(&h)) => h,
                _ => {
                    pos += 1;
                    continue;
                }
            };
            let next = pos + h.frame_len as u64;
            if next > file_len {
                break; // truncated last frame
            }
            // Confirm sync by checking what follows; guards against false 0xFFE patterns.
            let confirmed = next + 4 > file_len
                || r.get(next, 3).is_some_and(|b| b == b"TAG")
                || r.get(next, 8).is_some_and(|b| b == b"APETAGEX")
                || r.get(next, 4).and_then(parse_header).is_some_and(|n| h.compatible(&n));
            if !confirmed {
                pos += 1;
                continue;
            }
            let peek = r.get(pos, h.frame_len.min(64)).unwrap_or(&[]);
            if !is_tag_frame(peek, &h) {
                offsets.push(pos);
                min_kbps = min_kbps.min(h.bitrate_kbps);
                max_kbps = max_kbps.max(h.bitrate_kbps);
                sum_kbps += h.bitrate_kbps as u64;
                audio_end = next;
            }
            first.get_or_insert(h);
            pos = next;
        }

        let Some(first) = first else { bail!("no MPEG Layer III frames found") };
        if offsets.is_empty() {
            bail!("no MPEG audio frames found");
        }
        Ok(Mp3Index {
            avg_kbps: (sum_kbps / offsets.len() as u64) as u32,
            offsets,
            samples_per_frame: first.samples,
            sample_rate: first.sample_rate,
            channels: first.channels,
            min_kbps,
            max_kbps,
            audio_end,
        })
    }
}

fn id3v2_len(r: &mut Window) -> u64 {
    match r.get(0, 10) {
        Some(b) if &b[0..3] == b"ID3" => {
            let size = b[6..10].iter().fold(0u64, |acc, &x| (acc << 7) | (x & 0x7F) as u64);
            let footer = if b[5] & 0x10 != 0 { 10 } else { 0 };
            10 + size + footer
        }
        _ => 0,
    }
}

/// Buffered random access over a file without loading it whole (recordings can be 300 MB+).
struct Window {
    file: File,
    len: u64,
    buf: Vec<u8>,
    start: u64,
}

impl Window {
    const CHUNK: usize = 1 << 20;

    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len, buf: Vec::new(), start: 0 })
    }

    fn get(&mut self, pos: u64, n: usize) -> Option<&[u8]> {
        if pos + n as u64 > self.len {
            return None;
        }
        let end = self.start + self.buf.len() as u64;
        if pos < self.start || pos + n as u64 > end {
            self.file.seek(SeekFrom::Start(pos)).ok()?;
            let want = Self::CHUNK.max(n).min((self.len - pos) as usize);
            self.buf.resize(want, 0);
            self.file.read_exact(&mut self.buf).ok()?;
            self.start = pos;
        }
        let i = (pos - self.start) as usize;
        Some(&self.buf[i..i + n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_128k_44100_stereo() {
        let h = parse_header(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        assert_eq!(h.version, MpegVersion::V1);
        assert_eq!(h.bitrate_kbps, 128);
        assert_eq!(h.sample_rate, 44100);
        assert_eq!(h.channels, 2);
        assert_eq!(h.frame_len, 417);
        assert_eq!(h.samples, 1152);
    }

    #[test]
    fn rejects_non_layer3() {
        assert!(parse_header(&[0xFF, 0xFD, 0x90, 0x00]).is_none()); // layer II
        assert!(parse_header(&[0xFF, 0xFB, 0x0C, 0x00]).is_none()); // free format
    }
}
