//! Test fixtures: synthetic signals encoded to real MP3/WAV files.
#![allow(dead_code)]

use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, Quality, VbrMode};
use splitter_audio::source::PcmSource;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

pub const RATE: u32 = 44100;
pub const SECS: usize = 12;

#[derive(Clone, Copy)]
pub enum Mp3Mode {
    Cbr(Bitrate),
    Vbr,
}

/// Stereo test signal whose content changes constantly (chirp + LCG noise), so an
/// off-by-some-samples result can't match by accident.
pub fn signal() -> Vec<i16> {
    let n = RATE as usize * SECS;
    let mut out = Vec::with_capacity(n * 2);
    let mut seed: u32 = 12345;
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let chirp = (2.0 * std::f32::consts::PI * (200.0 + 300.0 * t) * t).sin();
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 16) as f32 / 65535.0 - 0.5;
        // Quiet gap in the middle, like the space between two tracks.
        let gain = if (5.0..6.0).contains(&t) { 0.0 } else { 0.5 };
        out.push((gain * (chirp * 0.8 + noise * 0.2) * 32767.0) as i16);
        out.push((gain * (chirp * 0.3 - noise * 0.4) * 32767.0) as i16);
    }
    out
}

pub fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("splitter-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

pub fn encode_mp3(path: &Path, mode: Mp3Mode) {
    let mut b = Builder::new().unwrap();
    b.set_num_channels(2).unwrap();
    b.set_sample_rate(RATE).unwrap();
    // Never let LAME resample (it would at low bitrates).
    b.set_output_sample_rate(NonZeroU32::new(RATE)).unwrap();
    match mode {
        Mp3Mode::Vbr => {
            b.set_vbr_mode(VbrMode::Mtrh).unwrap();
            b.set_vbr_quality(Quality::Good).unwrap();
        }
        Mp3Mode::Cbr(rate) => {
            b.set_vbr_mode(VbrMode::Off).unwrap();
            b.set_brate(rate).unwrap();
        }
    }
    b.set_quality(Quality::Good).unwrap();
    b.set_to_write_vbr_tag(true).unwrap();
    let mut enc = b.build().unwrap();

    let pcm = signal();
    let mut mp3 = Vec::new();
    // The *_to_vec helpers write into spare capacity only, and LAME treats an empty
    // buffer as unbounded, so always reserve first.
    for chunk in pcm.chunks(4096 * 2) {
        mp3.reserve(mp3lame_encoder::max_required_buffer_size(chunk.len()));
        enc.encode_to_vec(InterleavedPcm(chunk), &mut mp3).unwrap();
    }
    mp3.reserve(7200);
    enc.flush_to_vec::<FlushNoGap>(&mut mp3).unwrap();
    // Replace the placeholder first frame with the real Xing/Info + LAME tag.
    let mut tag = Vec::with_capacity(enc.lame_tag_size());
    if enc.lame_tag_encode_to_vec(&mut tag).is_some() {
        mp3[..tag.len()].copy_from_slice(&tag);
    }

    // Prepend a small ID3v2 tag, as real files have.
    let mut file = b"ID3\x03\x00\x00\x00\x00\x00\x0a".to_vec();
    file.extend_from_slice(&[0u8; 10]);
    file.extend_from_slice(&mp3);
    std::fs::write(path, file).unwrap();
}

pub fn write_wav(path: &Path) {
    let pcm = signal();
    let data_len = (pcm.len() * 2) as u32;
    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(36 + data_len).to_le_bytes());
    f.extend_from_slice(b"WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes()); // PCM
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&RATE.to_le_bytes());
    f.extend_from_slice(&(RATE * 4).to_le_bytes());
    f.extend_from_slice(&4u16.to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        f.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, f).unwrap();
}

pub fn decode_all(src: &mut dyn PcmSource) -> Vec<f32> {
    let mut all = Vec::new();
    while src.read(&mut all).unwrap() {}
    all
}

/// Decode with gapless trimming on (LAME delay/padding honoured), as a player would.
pub fn decode_gapless(path: &Path) -> Vec<f32> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::probe::Hint;
    let mss = MediaSourceStream::new(Box::new(std::fs::File::open(path).unwrap()), Default::default());
    let opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let mut format =
        symphonia::default::get_probe().format(&Hint::new(), mss, &opts, &Default::default()).unwrap().format;
    let track = format.default_track().unwrap().clone();
    let mut dec = symphonia::default::get_codecs().make(&track.codec_params, &Default::default()).unwrap();
    let mut out = Vec::new();
    while let Ok(packet) = format.next_packet() {
        let decoded = dec.decode(&packet).unwrap();
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        out.extend_from_slice(buf.samples());
    }
    out
}
