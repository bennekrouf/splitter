//! Re-encoded exports: lossless profiles decode back to the source exactly, MP3 comes back
//! with the exact length and sample-aligned, and A/B previews line up with the source.

mod common;

use common::*;
use mp3lame_encoder::Bitrate;
use splitter_audio::export::{export, ExportJob, Tags};
use splitter_audio::scan::scan;
use splitter_audio::source::open_source;
use splitter_audio::transcode::preview;
use splitter_core::export::Profile;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const A: u64 = 1_153 * 3 + 7;
const B: u64 = 5 * RATE as u64 + 22_050;

fn source_pcm(path: &Path) -> (Vec<f32>, usize) {
    let s = scan(path, &mut |_| {}).unwrap();
    let mut src = open_source(path, s.mp3.map(Arc::new)).unwrap();
    let ch = src.channels();
    (decode_all(src.as_mut()), ch)
}

fn export_one(source: &Path, profile: Profile, name: &str) -> PathBuf {
    let s = scan(source, &mut |_| {}).unwrap();
    let ext = profile.extension(source.extension().unwrap().to_str().unwrap());
    let out = tmp("transcode").join(format!("{name}.{ext}"));
    let job = ExportJob {
        start: A,
        end: B,
        path: out.clone(),
        gain_db: 0.0,
        tags: Tags { replaygain: None, title: "Été à Paris".into(), album: "Live".into(), track: 2, total: 9 },
    };
    export(source, &s, &[job], profile, &mut |_| {}).unwrap();
    out
}

/// Signal-to-noise ratio of `got` against `want`, in dB.
fn snr_db(got: &[f32], want: &[f32]) -> f64 {
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (g, w) in got.iter().zip(want) {
        sig += (*w as f64).powi(2);
        err += (*g as f64 - *w as f64).powi(2);
    }
    10.0 * (sig / err.max(1e-20)).log10()
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn wav_to_flac_is_lossless() {
    let src = tmp("transcode-src.wav");
    write_wav(&src);
    let (pcm, ch) = source_pcm(&src);
    let out = export_one(&src, Profile::Flac, "wav-flac");
    let got = decode_gapless(&out);
    let want = &pcm[A as usize * ch..B as usize * ch];
    assert_eq!(got.len(), want.len());
    assert!(max_diff(&got, want) < 1e-6, "FLAC must be bit-exact for a 16-bit source");
    // Smaller than the PCM it holds.
    assert!(std::fs::metadata(&out).unwrap().len() < want.len() as u64 * 2);
}

#[test]
fn wav_to_wav16_is_exact() {
    let src = tmp("transcode-src2.wav");
    write_wav(&src);
    let (pcm, ch) = source_pcm(&src);
    let got = decode_gapless(&export_one(&src, Profile::Wav16, "wav-wav16"));
    assert_eq!(got.len(), (B - A) as usize * ch);
    assert!(max_diff(&got, &pcm[A as usize * ch..B as usize * ch]) < 1e-6);
}

#[test]
fn mp3_to_flac_keeps_the_decoded_audio() {
    let src = tmp("transcode-src.mp3");
    encode_mp3(&src, Mp3Mode::Cbr(Bitrate::Kbps128));
    let (pcm, ch) = source_pcm(&src);
    let got = decode_gapless(&export_one(&src, Profile::Flac, "mp3-flac"));
    let want = &pcm[A as usize * ch..B as usize * ch];
    assert_eq!(got.len(), want.len());
    // Only 16-bit rounding of the decoded MP3.
    assert!(max_diff(&got, want) <= 1.0 / 32768.0 + 1e-6);
}

#[test]
fn reencoded_mp3_is_exact_length_and_aligned() {
    for (name, src_mode) in [("wav", None), ("mp3", Some(Mp3Mode::Vbr))] {
        let src = tmp(&format!("transcode-src-{name}-for-mp3.{name}"));
        match src_mode {
            None => write_wav(&src),
            Some(m) => encode_mp3(&src, m),
        }
        let (pcm, ch) = source_pcm(&src);
        for profile in [Profile::Mp3Vbr { quality: 2 }, Profile::Mp3Cbr { kbps: 192 }] {
            let out = export_one(&src, profile, &format!("{name}-{}", profile.short().replace(' ', "")));
            let got = decode_gapless(&out);
            let want = &pcm[A as usize * ch..B as usize * ch];
            assert_eq!(got.len(), want.len(), "{name} → {}: gapless length", profile.short());
            // Misaligned by even one sample, this noisy test signal would score near 0 dB.
            let snr = snr_db(&got, want);
            assert!(snr > 12.0, "{name} → {}: SNR {snr:.1} dB", profile.short());
            assert_eq!(&std::fs::read(&out).unwrap()[..3], b"ID3");
        }
    }
}

#[test]
fn previews_line_up_with_the_source() {
    let src = tmp("transcode-src-preview.wav");
    write_wav(&src);
    let (pcm, ch) = source_pcm(&src);
    let s = scan(&src, &mut |_| {}).unwrap();
    let want = &pcm[A as usize * ch..B as usize * ch];
    for profile in [Profile::Mp3Vbr { quality: 4 }, Profile::Flac, Profile::Wav16] {
        let mut source = open_source(&src, s.mp3.clone().map(Arc::new)).unwrap();
        let p = preview(profile, source.as_mut(), A, B, Some(16)).unwrap();
        assert_eq!(p.len(), want.len(), "{}", profile.short());
        let snr = snr_db(&p, want);
        let floor = if profile.is_lossy() { 12.0 } else { 80.0 };
        assert!(snr > floor, "{}: SNR {snr:.1} dB", profile.short());
    }
}
