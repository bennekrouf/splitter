//! Exported tracks must be exactly the source samples `[start, end)`: for MP3 when decoded
//! gaplessly (as players that honour the LAME header do), for WAV byte for byte.

mod common;

use common::*;
use mp3lame_encoder::Bitrate;
use splitter_audio::export::{export, ExportJob, Tags};
use splitter_audio::scan::scan;
use splitter_audio::source::open_source;
use std::path::Path;
use std::sync::Arc;

/// Samples every MP3 decoder emits before the first real one; a track starting at 0 can't
/// begin earlier than this.
const DECODER_DELAY: u64 = 529;

fn check_export(source: &Path, name: &str) {
    let s = scan(source, &mut |_| {}).unwrap();
    let total = s.info.total_samples;
    let mut src = open_source(source, s.mp3.clone().map(Arc::new)).unwrap();
    let ch = src.channels();
    let full = decode_all(src.as_mut());

    // Awkward boundaries: mid-frame, on a frame edge, in loud audio, in the quiet gap, the end.
    let bounds = [0, 1_153 * 3 + 7, 99_999, 115_200, 5 * RATE as u64 + 22_050, 400_001, total];
    let dir = tmp(&format!("export-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let ext = source.extension().unwrap().to_str().unwrap();
    let jobs: Vec<ExportJob> = bounds
        .windows(2)
        .enumerate()
        .map(|(i, w)| ExportJob {
            start: w[0],
            end: w[1],
            path: dir.join(format!("{:02}.{ext}", i + 1)),
            tags: Tags { title: format!("Track {}", i + 1), album: "Test".into(), track: i + 1, total: 6 },
        })
        .collect();
    let mut done = 0;
    export(source, &s, &jobs, &mut |n| done = n).unwrap();
    assert_eq!(done, jobs.len());

    for job in &jobs {
        let got = decode_gapless(&job.path);
        let start = if s.mp3.is_some() { job.start.max(DECODER_DELAY) } else { job.start };
        let want = &full[start as usize * ch..job.end as usize * ch];
        assert_eq!(
            got.len(),
            want.len(),
            "{}: {} frames, expected {}",
            job.path.display(),
            got.len() / ch,
            want.len() / ch
        );
        let max_diff = got.iter().zip(want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_diff < 1e-4, "{}: samples differ (max {max_diff})", job.path.display());
    }
}

#[test]
fn mp3_cbr_export_is_sample_exact() {
    let path = tmp("export-src-cbr.mp3");
    encode_mp3(&path, Mp3Mode::Cbr(Bitrate::Kbps128));
    check_export(&path, "cbr");
}

#[test]
fn mp3_vbr_export_is_sample_exact() {
    let path = tmp("export-src-vbr.mp3");
    encode_mp3(&path, Mp3Mode::Vbr);
    check_export(&path, "vbr");
}

#[test]
fn mp3_low_bitrate_export_is_sample_exact() {
    let path = tmp("export-src-cbr32.mp3");
    encode_mp3(&path, Mp3Mode::Cbr(Bitrate::Kbps32));
    check_export(&path, "cbr32");
}

#[test]
fn wav_export_is_byte_exact() {
    let path = tmp("export-src.wav");
    write_wav(&path);
    check_export(&path, "wav");
    // Byte-level check on one track: the data chunk is a straight copy.
    let src = std::fs::read(&path).unwrap();
    let out = std::fs::read(tmp("export-wav").join("02.wav")).unwrap();
    let (a, b) = (1_153 * 3 + 7, 99_999);
    assert_eq!(&out[44..], &src[44 + a * 4..44 + b * 4]);
}
