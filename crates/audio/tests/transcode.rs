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

/// Apply cuts: the kept ranges come out back to back, sample for sample, from WAV and MP3.
#[test]
fn kept_ranges_are_written_back_to_back() {
    use splitter_audio::transcode::write_ranges_to_wav;
    let wav = tmp("cleaned-src.wav");
    write_wav(&wav);
    let mp3 = tmp("cleaned-src.mp3");
    encode_mp3(&mp3, Mp3Mode::Cbr(Bitrate::Kbps128));
    let ranges = [(1_000, 30_000), (40_000, 41_000), (70_000, 100_000)];
    for (src, name) in [(&wav, "cleaned-wav.wav"), (&mp3, "cleaned-mp3.wav")] {
        let (pcm, ch) = source_pcm(src);
        let s = scan(src, &mut |_| {}).unwrap();
        let out = tmp(name);
        write_ranges_to_wav(src, &s, &ranges, &out, &mut |_| {}).unwrap();
        let want: Vec<f32> = ranges.iter().flat_map(|&(a, b)| pcm[a as usize * ch..b as usize * ch].to_vec()).collect();
        let (got, got_ch) = source_pcm(&out);
        assert_eq!(got_ch, ch);
        assert_eq!(got.len(), want.len(), "{name}: length");
        // 16-bit output: within one quantization step of the source.
        assert!(max_diff(&got, &want) <= 1.0 / 32768.0 + 1e-6, "{name}: {}", max_diff(&got, &want));
    }
}

/// Audio inside a video can't be copied as is; re-encoding it cuts exactly `[A, B)`.
#[test]
fn video_audio_exports_by_reencoding() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video.mp4");
    let s = scan(&src, &mut |_| {}).unwrap();
    assert!(!splitter_audio::export::can_copy(&src, &s));
    let job = |path: PathBuf| ExportJob {
        start: A,
        end: B,
        path,
        gain_db: 0.0,
        tags: Tags { replaygain: None, title: "Clip".into(), album: "Video".into(), track: 1, total: 1 },
    };
    let original = tmp("transcode").join("video-original.mp4");
    assert!(export(&src, &s, &[job(original.clone())], Profile::Original, &mut |_| {}).is_err());
    assert!(!original.exists());

    let (pcm, ch) = source_pcm(&src);
    let out = export_one(&src, Profile::Flac, "video-flac");
    let got = decode_gapless(&out);
    let want = &pcm[A as usize * ch..B as usize * ch];
    assert_eq!(got.len(), want.len());
    assert!(max_diff(&got, want) < 1.0 / 32768.0 * 1.5, "FLAC of the decoded AAC must match it");
}
