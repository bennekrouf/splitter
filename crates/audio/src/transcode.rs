//! Re-encoding tracks to another format (MP3 via LAME, FLAC, 16-bit WAV), short previews
//! of what an encoding sounds like, for A/B listening, and whole-file conversion to WAV.
//!
//! Input always comes from a `PcmSource`, so it is sample-exact in the same numbering as the
//! splits. MP3 output carries LAME's own gapless header, so players trim it back to exactly the
//! track's samples.

use crate::export::{id3v2, Tags, WavLayout};
use crate::scan::Scan;
use crate::source::{open_source, GenericSource, PcmSource};
use anyhow::{anyhow, bail, Context, Result};
use flacenc::component::BitRepr;
use flacenc::error::Verify;
use mp3lame_encoder::{Bitrate, Builder, FlushGap, InterleavedPcm, MonoPcm, Quality, VbrMode};
use splitter_core::export::Profile;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

/// Sample rates MP3 can carry; anything else is resampled by LAME.
const MP3_RATES: [u32; 9] = [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000];

/// Feed `[start, end)` from `src` to `sink` in chunks of interleaved samples.
fn read_range(src: &mut dyn PcmSource, start: u64, end: u64, mut sink: impl FnMut(&[f32]) -> Result<()>) -> Result<()> {
    let ch = src.channels();
    src.seek(start)?;
    let mut left = (end.saturating_sub(start)) as usize * ch;
    let mut buf = Vec::with_capacity(8192);
    while left > 0 {
        buf.clear();
        if !src.read(&mut buf)? {
            bail!("the recording ended {} samples early", left / ch);
        }
        let take = buf.len().min(left);
        sink(&buf[..take])?;
        left -= take;
    }
    Ok(())
}

/// Scales another source's samples (normalization).
struct Gain<'a> {
    inner: &'a mut dyn PcmSource,
    factor: f32,
}

impl PcmSource for Gain<'_> {
    fn channels(&self) -> usize {
        self.inner.channels()
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn seek(&mut self, frame: u64) -> Result<()> {
        self.inner.seek(frame)
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        let from = out.len();
        let more = self.inner.read(out)?;
        out[from..].iter_mut().for_each(|x| *x *= self.factor);
        Ok(more)
    }
}

/// Encode `[start, end)` of `src` with `profile` into `out`, `gain_db` louder. `bits` is the
/// source's bit depth for PCM sources (used by FLAC), `None` for MP3 sources.
#[allow(clippy::too_many_arguments)]
pub fn encode_track(
    profile: Profile,
    src: &mut dyn PcmSource,
    start: u64,
    end: u64,
    bits: Option<u32>,
    gain_db: f64,
    tags: &Tags,
    out: &mut dyn Write,
) -> Result<()> {
    let mut gained;
    let src: &mut dyn PcmSource = if gain_db.abs() > 1e-6 {
        gained = Gain { inner: src, factor: 10f64.powf(gain_db / 20.0) as f32 };
        &mut gained
    } else {
        src
    };
    match profile {
        Profile::Original => bail!("Original is copied, not encoded"),
        Profile::Mp3Vbr { .. } | Profile::Mp3Cbr { .. } => {
            let mp3 = encode_mp3(profile, src, start, end)?;
            out.write_all(&id3v2(tags))?;
            out.write_all(&mp3)?;
        }
        Profile::Flac => encode_flac(src, start, end, flac_bits(bits), tags, out)?,
        Profile::Wav16 => encode_wav16(src, start, end, out)?,
    }
    Ok(())
}

fn flac_bits(bits: Option<u32>) -> usize {
    match bits {
        Some(b) if b > 16 => 24,
        _ => 16,
    }
}

// ---------------------------------------------------------------------------------------------
// MP3

fn encode_mp3(profile: Profile, src: &mut dyn PcmSource, start: u64, end: u64) -> Result<Vec<u8>> {
    let ch = src.channels();
    let rate = src.sample_rate();
    let out_ch = ch.min(2);
    let mut b = Builder::new().ok_or_else(|| anyhow!("could not start the MP3 encoder"))?;
    b.set_num_channels(out_ch as u8).map_err(|e| anyhow!("MP3 channels: {e:?}"))?;
    b.set_sample_rate(rate).map_err(|e| anyhow!("MP3 sample rate: {e:?}"))?;
    // Keep the rate when MP3 can carry it; otherwise the nearest family (88.2k → 44.1k, 96k → 48k).
    let out_rate = match rate {
        r if MP3_RATES.contains(&r) => r,
        r if r % 44100 == 0 => 44100,
        _ => 48000,
    };
    b.set_output_sample_rate(NonZeroU32::new(out_rate)).map_err(|e| anyhow!("MP3 output rate: {e:?}"))?;
    match profile {
        Profile::Mp3Vbr { quality } => {
            b.set_vbr_mode(VbrMode::Mtrh).map_err(|e| anyhow!("{e:?}"))?;
            b.set_vbr_quality(lame_quality(quality)).map_err(|e| anyhow!("{e:?}"))?;
        }
        Profile::Mp3Cbr { kbps } => {
            b.set_vbr_mode(VbrMode::Off).map_err(|e| anyhow!("{e:?}"))?;
            b.set_brate(lame_bitrate(kbps)).map_err(|e| anyhow!("{e:?}"))?;
        }
        _ => unreachable!(),
    }
    // LAME's recommended algorithm quality (-q 2).
    b.set_quality(Quality::NearBest).map_err(|e| anyhow!("{e:?}"))?;
    b.set_to_write_vbr_tag(true).map_err(|e| anyhow!("{e:?}"))?;
    let mut enc = b.build().map_err(|e| anyhow!("MP3 encoder: {e:?}"))?;

    let mut mp3 = Vec::new();
    let mut stereo = Vec::new();
    read_range(src, start, end, |chunk| {
        // The *_to_vec helpers write into spare capacity only, and LAME treats an empty buffer
        // as unbounded, so always reserve first.
        mp3.reserve(mp3lame_encoder::max_required_buffer_size(chunk.len() / ch));
        let r = match ch {
            1 => enc.encode_to_vec(MonoPcm(chunk), &mut mp3),
            2 => enc.encode_to_vec(InterleavedPcm(chunk), &mut mp3),
            _ => {
                // More than two channels: keep the first two.
                stereo.clear();
                stereo.extend(chunk.chunks_exact(ch).flat_map(|f| [f[0], f[1]]));
                enc.encode_to_vec(InterleavedPcm(&stereo[..]), &mut mp3)
            }
        };
        r.map(|_| ()).map_err(|e| anyhow!("MP3 encoding: {e:?}"))
    })?;
    mp3.reserve(7200);
    enc.flush_to_vec::<FlushGap>(&mut mp3).map_err(|e| anyhow!("MP3 flush: {e:?}"))?;
    // LAME reserves the first frame for its header; fill it in now that totals are known.
    let mut tag = Vec::with_capacity(enc.lame_tag_size());
    if enc.lame_tag_encode_to_vec(&mut tag).is_some() && tag.len() <= mp3.len() {
        mp3[..tag.len()].copy_from_slice(&tag);
    }
    Ok(mp3)
}

fn lame_quality(q: u8) -> Quality {
    match q {
        0 => Quality::Best,
        1 => Quality::SecondBest,
        2 => Quality::NearBest,
        3 => Quality::VeryNice,
        4 => Quality::Nice,
        5 => Quality::Good,
        6 => Quality::Decent,
        7 => Quality::Ok,
        8 => Quality::SecondWorst,
        _ => Quality::Worst,
    }
}

fn lame_bitrate(kbps: u16) -> Bitrate {
    match kbps {
        0..=96 => Bitrate::Kbps96,
        97..=112 => Bitrate::Kbps112,
        113..=128 => Bitrate::Kbps128,
        129..=160 => Bitrate::Kbps160,
        161..=192 => Bitrate::Kbps192,
        193..=224 => Bitrate::Kbps224,
        225..=256 => Bitrate::Kbps256,
        _ => Bitrate::Kbps320,
    }
}

// ---------------------------------------------------------------------------------------------
// FLAC

fn to_int(x: f32, bits: usize) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    (x * (max + 1.0)).round().clamp(-max - 1.0, max) as i32
}

/// Streams a `PcmSource` range into flacenc as integers. The encoder owns the feed, so read
/// errors are parked in `error` for the caller.
struct FlacFeed<'a> {
    src: &'a mut dyn PcmSource,
    bits: usize,
    left: usize,
    buf: Vec<f32>,
    at: usize,
    ints: Vec<i32>,
    error: &'a mut Option<anyhow::Error>,
}

impl flacenc::source::Source for FlacFeed<'_> {
    fn channels(&self) -> usize {
        self.src.channels()
    }

    fn bits_per_sample(&self) -> usize {
        self.bits
    }

    fn sample_rate(&self) -> usize {
        self.src.sample_rate() as usize
    }

    fn read_samples<F: flacenc::source::Fill>(
        &mut self,
        block_size: usize,
        dest: &mut F,
    ) -> Result<usize, flacenc::error::SourceError> {
        let ch = self.src.channels();
        self.ints.clear();
        while self.ints.len() < block_size * ch && self.left > 0 {
            if self.at >= self.buf.len() {
                self.buf.clear();
                self.at = 0;
                match self.src.read(&mut self.buf) {
                    Ok(true) => {}
                    Ok(false) => {
                        *self.error = Some(anyhow!("the recording ended early"));
                        break;
                    }
                    Err(e) => {
                        *self.error = Some(e);
                        break;
                    }
                }
            }
            let take = (self.buf.len() - self.at).min(block_size * ch - self.ints.len()).min(self.left);
            let bits = self.bits;
            self.ints.extend(self.buf[self.at..self.at + take].iter().map(|&x| to_int(x, bits)));
            self.at += take;
            self.left -= take;
        }
        dest.fill_interleaved(&self.ints)?;
        Ok(self.ints.len() / ch)
    }

    fn len_hint(&self) -> Option<usize> {
        Some(self.left / self.src.channels())
    }
}

fn encode_flac(
    src: &mut dyn PcmSource,
    start: u64,
    end: u64,
    bits: usize,
    tags: &Tags,
    out: &mut dyn Write,
) -> Result<()> {
    let ch = src.channels();
    src.seek(start)?;
    let config =
        flacenc::config::Encoder::default().into_verified().map_err(|(_, e)| anyhow!("FLAC settings: {e:?}"))?;
    let mut error = None;
    let feed = FlacFeed {
        src,
        bits,
        left: (end - start) as usize * ch,
        buf: Vec::with_capacity(8192),
        at: 0,
        ints: Vec::new(),
        error: &mut error,
    };
    let block = config.block_size;
    let mut stream =
        flacenc::encode_with_fixed_block_size(&config, feed, block).map_err(|e| anyhow!("FLAC encoding: {e:?}"))?;
    if let Some(e) = error {
        return Err(e.context("reading audio for FLAC"));
    }
    let comments = vorbis_comment(tags);
    stream.add_metadata_block(
        flacenc::component::MetadataBlockData::new_unknown(4, &comments).map_err(|e| anyhow!("{e:?}"))?,
    );
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| anyhow!("FLAC output: {e:?}"))?;
    let mut bytes = sink.as_slice().to_vec();
    // flacenc records the short final block as the stream's minimum block size. The spec
    // excludes the last block, and decoders (symphonia among them) read min != max as
    // "variable block size" and then reject the fixed-size frames. STREAMINFO starts at byte 8:
    // min block size (u16), max block size (u16).
    if bytes.len() >= 12 && &bytes[..4] == b"fLaC" {
        let max = [bytes[10], bytes[11]];
        bytes[8..10].copy_from_slice(&max);
    }
    out.write_all(&bytes)?;
    Ok(())
}

/// FLAC VORBIS_COMMENT block body.
fn vorbis_comment(tags: &Tags) -> Vec<u8> {
    let mut fields = Vec::new();
    if !tags.title.is_empty() {
        fields.push(format!("TITLE={}", tags.title));
    }
    if !tags.album.is_empty() {
        fields.push(format!("ALBUM={}", tags.album));
    }
    if tags.track > 0 {
        fields.push(format!("TRACKNUMBER={}", tags.track));
        fields.push(format!("TRACKTOTAL={}", tags.total));
    }
    for (key, value) in tags.replaygain.iter().flat_map(|rg| rg.fields()) {
        fields.push(format!("{key}={value}"));
    }
    let vendor = b"splitter";
    let mut out = Vec::new();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor);
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    for f in fields {
        out.extend_from_slice(&(f.len() as u32).to_le_bytes());
        out.extend_from_slice(f.as_bytes());
    }
    out
}

// ---------------------------------------------------------------------------------------------
// WAV

fn encode_wav16(src: &mut dyn PcmSource, start: u64, end: u64, out: &mut dyn Write) -> Result<()> {
    let ch = src.channels() as u32;
    let rate = src.sample_rate();
    let data_len = (end - start) * ch as u64 * 2;
    wav16_header(out, ch, rate, data_len)?;
    let mut bytes = Vec::with_capacity(16384);
    read_range(src, start, end, |chunk| {
        bytes.clear();
        bytes.extend(chunk.iter().flat_map(|&x| (to_int(x, 16) as i16).to_le_bytes()));
        out.write_all(&bytes).context("writing WAV")
    })
}

fn wav16_header(out: &mut dyn Write, ch: u32, rate: u32, data_len: u64) -> Result<()> {
    if 36 + data_len > u32::MAX as u64 {
        bail!("audio is larger than 4 GB, which WAV can't hold");
    }
    out.write_all(b"RIFF")?;
    out.write_all(&((36 + data_len) as u32).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&(ch as u16).to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * ch * 2).to_le_bytes())?;
    out.write_all(&((ch * 2) as u16).to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&(data_len as u32).to_le_bytes())?;
    Ok(())
}

/// Decode any file symphonia reads (e.g. a downloaded AAC .m4a) into a 16-bit WAV at `dst`.
/// Written to a temporary name first, so a half-written file never shows up as a recording.
pub fn decode_to_wav(src: &Path, dst: &Path, progress: &mut dyn FnMut(f32)) -> Result<()> {
    let mut source = GenericSource::open(src)?;
    let (ch, rate) = (source.channels() as u32, source.sample_rate());
    let total = source.total_frames();
    let tmp = dst.with_extension("wav.part");
    let mut out = BufWriter::new(File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?);
    // Sizes are patched in once the length is known.
    wav16_header(&mut out, ch, rate, 0)?;
    let (mut buf, mut bytes, mut frames) = (Vec::new(), Vec::new(), 0u64);
    while source.read(&mut buf)? {
        bytes.clear();
        bytes.extend(buf.iter().flat_map(|&x| (to_int(x, 16) as i16).to_le_bytes()));
        out.write_all(&bytes).context("writing WAV")?;
        frames += (buf.len() / ch as usize) as u64;
        buf.clear();
        if let Some(t) = total.filter(|&t| t > 0) {
            progress((frames as f64 / t as f64).min(1.0) as f32);
        }
    }
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(0))?;
    wav16_header(&mut file, ch, rate, frames * ch as u64 * 2)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, dst).with_context(|| format!("renaming to {}", dst.display()))
}

/// Write the `ranges` (source frames, sorted) of a recording back to back into a new WAV at
/// `dst`: a lossless byte copy for a WAV source, 16-bit PCM decoded from anything else (MP3).
/// Written to a temporary name first, so a half-written file never shows up as a recording.
pub fn write_ranges_to_wav(
    path: &Path,
    scan: &Scan,
    ranges: &[(u64, u64)],
    dst: &Path,
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    let total: u64 = ranges.iter().map(|(a, b)| b - a).sum();
    if total == 0 {
        bail!("nothing to keep");
    }
    let tmp = dst.with_extension("wav.part");
    let result = (|| {
        let mut out = BufWriter::new(File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?);
        let mut done = 0u64;
        let mut step = |n: u64| {
            done += n;
            progress((done as f64 / total as f64) as f32);
        };
        let mut src = File::open(path)?;
        match WavLayout::read(&mut src).ok().filter(|_| scan.mp3.is_none()) {
            Some(l) => {
                let len = total * l.block_align;
                let fmt_len = l.fmt.len() as u64;
                let riff_len = 4 + (8 + fmt_len + (fmt_len & 1)) + (8 + len + (len & 1));
                if riff_len > u32::MAX as u64 {
                    bail!("the result is larger than 4 GB, which WAV can't hold");
                }
                out.write_all(b"RIFF")?;
                out.write_all(&(riff_len as u32).to_le_bytes())?;
                out.write_all(b"WAVEfmt ")?;
                out.write_all(&(fmt_len as u32).to_le_bytes())?;
                out.write_all(&l.fmt)?;
                if fmt_len & 1 == 1 {
                    out.write_all(&[0])?;
                }
                out.write_all(b"data")?;
                out.write_all(&(len as u32).to_le_bytes())?;
                let frames = l.data_len / l.block_align;
                for &(a, b) in ranges {
                    let (a, b) = (a.min(frames), b.min(frames));
                    src.seek(SeekFrom::Start(l.data_offset + a * l.block_align))?;
                    std::io::copy(&mut std::io::Read::take(&mut src, (b - a) * l.block_align), &mut out)?;
                    step(b - a);
                }
                if len & 1 == 1 {
                    out.write_all(&[0])?;
                }
            }
            None => {
                let mut source = open_source(path, scan.mp3.clone().map(Arc::new))?;
                let ch = source.channels() as u32;
                wav16_header(&mut out, ch, source.sample_rate(), total * ch as u64 * 2)?;
                let mut bytes = Vec::with_capacity(16384);
                for &(a, b) in ranges {
                    read_range(source.as_mut(), a, b, |chunk| {
                        bytes.clear();
                        bytes.extend(chunk.iter().flat_map(|&x| (to_int(x, 16) as i16).to_le_bytes()));
                        out.write_all(&bytes).context("writing WAV")?;
                        step((chunk.len() / ch as usize) as u64);
                        Ok(())
                    })?;
                }
            }
        }
        let file = out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, dst).with_context(|| format!("renaming to {}", dst.display()))
}

// ---------------------------------------------------------------------------------------------
// Preview

/// What `[start, end)` sounds like after encoding with `profile`, as interleaved samples at the
/// source's rate and channel count, sample-aligned with the source.
pub fn preview(profile: Profile, src: &mut dyn PcmSource, start: u64, end: u64, bits: Option<u32>) -> Result<Vec<f32>> {
    let ch = src.channels();
    let want = (end - start) as usize * ch;
    match profile {
        Profile::Mp3Vbr { .. } | Profile::Mp3Cbr { .. } => {
            if ch > 2 {
                bail!("A/B needs a mono or stereo source");
            }
            let mp3 = encode_mp3(profile, src, start, end)?;
            let pcm = decode_gapless(mp3)?;
            if pcm.len().abs_diff(want) > ch * 2 {
                bail!("the encoded preview doesn't line up with the source (resampled?)");
            }
            let mut pcm = pcm;
            pcm.resize(want, 0.0);
            Ok(pcm)
        }
        // Lossless (apart from word length): the preview is the source at the output's bit depth.
        Profile::Flac | Profile::Wav16 | Profile::Original => {
            let bits = match profile {
                Profile::Wav16 => 16,
                Profile::Flac => flac_bits(bits),
                _ => 24,
            };
            let scale = (1i64 << (bits - 1)) as f32;
            let mut pcm = Vec::with_capacity(want);
            read_range(src, start, end, |c| {
                pcm.extend(c.iter().map(|&x| to_int(x, bits) as f32 / scale));
                Ok(())
            })?;
            Ok(pcm)
        }
    }
}

/// Decode an in-memory MP3 honouring its LAME delay/padding.
fn decode_gapless(bytes: Vec<u8>) -> Result<Vec<f32>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let mut format =
        symphonia::default::get_probe().format(&Default::default(), mss, &opts, &Default::default())?.format;
    let track = format.default_track().ok_or_else(|| anyhow!("no track"))?.clone();
    let mut dec = symphonia::default::get_codecs().make(&track.codec_params, &Default::default())?;
    let mut out = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        let decoded = dec.decode(&packet)?;
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        out.extend_from_slice(buf.samples());
    }
    Ok(out)
}
