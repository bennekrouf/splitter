//! Seekable PCM sources for playback.
//!
//! MP3 seeks go through our frame index: reopen the demuxer far enough before the target to
//! refill the bit reservoir and MDCT overlap (`Mp3Index::decode_start`), then discard up to
//! the exact sample.
//! Other formats use symphonia's accurate seek.

use crate::mp3index::Mp3Index;
use anyhow::{anyhow, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::formats::MpaReader;

pub trait PcmSource: Send {
    fn channels(&self) -> usize;
    fn sample_rate(&self) -> u32;
    /// Position the source so the next `read` starts at sample frame `frame`.
    fn seek(&mut self, frame: u64) -> Result<()>;
    /// Append the next chunk of interleaved f32 samples. Returns `false` at end of stream.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool>;
}

pub fn open_source(path: &Path, mp3: Option<Arc<Mp3Index>>) -> Result<Box<dyn PcmSource>> {
    Ok(match mp3 {
        Some(index) => Box::new(Mp3Source::new(path, index)?),
        None => Box::new(GenericSource::open(path)?),
    })
}

/// A demuxer + decoder pair that yields interleaved f32, skipping a leading number of frames.
pub(crate) struct Decoding {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    buf: Option<SampleBuffer<f32>>,
    discard: u64,
}

impl Decoding {
    pub(crate) fn probe(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .context("unsupported or corrupt audio file")?;
        Self::from_format(probed.format)
    }

    fn from_format(format: Box<dyn FormatReader>) -> Result<Self> {
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| anyhow!("no audio track"))?;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .context("unsupported codec")?;
        Ok(Self { track_id: track.id, format, decoder, buf: None, discard: 0 })
    }

    pub(crate) fn format(&self) -> &dyn FormatReader {
        self.format.as_ref()
    }

    pub(crate) fn read(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymError::DecodeError(_)) => continue, // corrupt frame: skip it
                Err(e) => return Err(e.into()),
            };
            let spec = *decoded.spec();
            let channels = spec.channels.count();
            let needed = decoded.capacity() as u64;
            if self.buf.as_ref().is_none_or(|b| (b.capacity() as u64) < needed * channels as u64) {
                self.buf = Some(SampleBuffer::new(needed, spec));
            }
            let buf = self.buf.as_mut().unwrap();
            buf.copy_interleaved_ref(decoded);
            let samples = buf.samples();
            let frames = (samples.len() / channels) as u64;
            let skip = self.discard.min(frames);
            self.discard -= skip;
            if skip < frames {
                out.extend_from_slice(&samples[skip as usize * channels..]);
                return Ok(true);
            }
        }
    }
}

pub struct Mp3Source {
    path: PathBuf,
    index: Arc<Mp3Index>,
    dec: Option<Decoding>,
}

impl Mp3Source {
    pub fn new(path: &Path, index: Arc<Mp3Index>) -> Result<Self> {
        let mut s = Self { path: path.to_owned(), index, dec: None };
        s.seek(0)?;
        Ok(s)
    }
}

impl PcmSource for Mp3Source {
    fn channels(&self) -> usize {
        self.index.channels as usize
    }

    fn sample_rate(&self) -> u32 {
        self.index.sample_rate
    }

    fn seek(&mut self, frame: u64) -> Result<()> {
        let spf = self.index.samples_per_frame as u64;
        let target = (frame / spf) as usize;
        if target >= self.index.frames() {
            self.dec = None;
            return Ok(());
        }
        let start = self.index.decode_start(&mut File::open(&self.path)?, target)?;
        let from = self.index.offsets[start];
        let src = FileSlice::open(&self.path, from, self.index.audio_end)?;
        let mss = MediaSourceStream::new(Box::new(src), Default::default());
        let reader = MpaReader::try_new(mss, &FormatOptions::default())?;
        let mut dec = Decoding::from_format(Box::new(reader))?;
        dec.discard = frame - start as u64 * spf;
        self.dec = Some(dec);
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        match &mut self.dec {
            Some(d) => d.read(out),
            None => Ok(false),
        }
    }
}

pub struct GenericSource {
    dec: Decoding,
    channels: usize,
    rate: u32,
}

impl GenericSource {
    pub fn open(path: &Path) -> Result<Self> {
        let dec = Decoding::probe(path)?;
        let params = &dec.format().tracks().iter().find(|t| t.id == dec.track_id).unwrap().codec_params;
        let channels = params.channels.map(|c| c.count()).unwrap_or(2);
        let rate = params.sample_rate.ok_or_else(|| anyhow!("unknown sample rate"))?;
        Ok(Self { dec, channels, rate })
    }
}

impl PcmSource for GenericSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn seek(&mut self, frame: u64) -> Result<()> {
        let seeked =
            self.dec.format.seek(SeekMode::Accurate, SeekTo::TimeStamp { ts: frame, track_id: self.dec.track_id })?;
        self.dec.decoder.reset();
        self.dec.discard = seeked.required_ts.saturating_sub(seeked.actual_ts);
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        self.dec.read(out)
    }
}

/// Decoded audio held in memory covering source frames `[start, start + len)`, e.g. an
/// encoded A/B preview. Reads outside that window are silent.
pub struct MemPcm {
    pcm: Arc<Vec<f32>>,
    channels: usize,
    rate: u32,
    start: u64,
    pos: u64,
}

impl MemPcm {
    pub fn new(pcm: Arc<Vec<f32>>, channels: usize, rate: u32, start: u64) -> Self {
        Self { pcm, channels, rate, start, pos: start }
    }

    fn end(&self) -> u64 {
        self.start + (self.pcm.len() / self.channels) as u64
    }
}

impl PcmSource for MemPcm {
    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn seek(&mut self, frame: u64) -> Result<()> {
        self.pos = frame;
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        const CHUNK: u64 = 4096;
        if self.pos >= self.end() {
            return Ok(false);
        }
        if self.pos < self.start {
            let n = CHUNK.min(self.start - self.pos);
            out.extend(std::iter::repeat_n(0.0, n as usize * self.channels));
            self.pos += n;
            return Ok(true);
        }
        let n = CHUNK.min(self.end() - self.pos);
        let from = (self.pos - self.start) as usize * self.channels;
        out.extend_from_slice(&self.pcm[from..from + n as usize * self.channels]);
        self.pos += n;
        Ok(true)
    }
}

/// A byte range `[start, end)` of a file presented as a whole stream. Reported as
/// non-seekable so `MpaReader` doesn't scan the file to estimate its duration.
struct FileSlice {
    file: File,
    start: u64,
    len: u64,
    pos: u64,
}

impl FileSlice {
    fn open(path: &Path, start: u64, end: u64) -> Result<Self> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(start))?;
        Ok(Self { file, start, len: end.saturating_sub(start), pos: 0 })
    }
}

impl Read for FileSlice {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.len.saturating_sub(self.pos) as usize;
        let take = buf.len().min(left);
        let n = self.file.read(&mut buf[..take])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for FileSlice {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => self.len as i64 + d,
        };
        if target < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = target as u64;
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        Ok(self.pos)
    }
}

impl MediaSource for FileSlice {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}
