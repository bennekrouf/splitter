//! End-to-end checks on real encoded files: the frame index agrees with symphonia's sample
//! numbering, and seeking lands on exactly the samples a straight decode produces.

mod common;

use common::*;
use mp3lame_encoder::Bitrate;
use splitter_audio::scan::scan;
use splitter_audio::source::open_source;
use std::path::Path;
use std::sync::Arc;

fn check_seeks(path: &Path, expect_frames_close_to: u64) {
    let scan = scan(path, &mut |_| {}).unwrap();
    let total = scan.info.total_samples;
    assert!(
        total.abs_diff(expect_frames_close_to) < 4000,
        "{}: decoded {total} frames, expected ~{expect_frames_close_to}",
        path.display()
    );
    if let Some(index) = &scan.mp3 {
        assert_eq!(index.total_samples(), total, "frame index disagrees with symphonia");
    }

    let mp3 = scan.mp3.clone().map(Arc::new);
    let mut src = open_source(path, mp3.clone()).unwrap();
    let ch = src.channels();
    let full = decode_all(src.as_mut());
    assert_eq!(full.len() as u64, total * ch as u64);

    let targets = [0, 1, 1151, 1152, 1153, 4000, 50_000, 3 * RATE as u64 + 17, total - 5000, total - 1];
    for &t in &targets {
        let mut s = open_source(path, mp3.clone()).unwrap();
        s.seek(t).unwrap();
        let mut got = Vec::new();
        while got.len() < 4096 * ch && s.read(&mut got).unwrap() {}
        let want = &full[t as usize * ch..];
        let n = got.len().min(want.len()).min(4096 * ch);
        assert!(n > 0 || t >= total, "{}: nothing decoded after seek to {t}", path.display());
        let max_diff = got[..n].iter().zip(&want[..n]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_diff < 1e-4, "{}: seek to {t} is off (max diff {max_diff})", path.display());
    }

    // The quiet gap at 5–6 s must show up in the loudness track.
    let w = scan.loudness.window;
    let gap = scan.loudness.db[(5.3 * RATE as f64) as usize / w as usize];
    let loud = scan.loudness.db[(3.0 * RATE as f64) as usize / w as usize];
    assert!(gap < -60.0 && loud > -30.0, "loudness gap {gap} dB, loud {loud} dB");
}

#[test]
fn mp3_cbr_index_and_seek_are_exact() {
    let path = tmp("cbr.mp3");
    encode_mp3(&path, Mp3Mode::Cbr(Bitrate::Kbps128));
    check_seeks(&path, RATE as u64 * SECS as u64);
    let scan = scan(&path, &mut |_| {}).unwrap();
    assert!(matches!(scan.info.bitrate, splitter_audio::Bitrate::Cbr(128)));
}

#[test]
fn mp3_vbr_index_and_seek_are_exact() {
    let path = tmp("vbr.mp3");
    encode_mp3(&path, Mp3Mode::Vbr);
    check_seeks(&path, RATE as u64 * SECS as u64);
    let scan = scan(&path, &mut |_| {}).unwrap();
    assert!(matches!(scan.info.bitrate, splitter_audio::Bitrate::Vbr { .. }), "{:?}", scan.info.bitrate);
}

#[test]
fn mp3_low_bitrate_seek_is_exact() {
    // At 32 kbps the bit reservoir reaches back several frames.
    let path = tmp("cbr32.mp3");
    encode_mp3(&path, Mp3Mode::Cbr(Bitrate::Kbps32));
    check_seeks(&path, RATE as u64 * SECS as u64);
}

#[test]
fn wav_seek_is_exact() {
    let path = tmp("pcm.wav");
    write_wav(&path);
    check_seeks(&path, RATE as u64 * SECS as u64);
}
