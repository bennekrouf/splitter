//! Range loudness derived from the stored blocks must agree with measuring the range directly.

mod common;

use common::*;
use ebur128::{EbuR128, Mode};
use splitter_audio::loudness::analyze;
use splitter_audio::scan::scan;
use splitter_audio::source::open_source;
use std::sync::Arc;

#[test]
fn range_loudness_matches_direct_measurement() {
    let path = tmp("loudness-src.wav");
    write_wav(&path);
    let s = scan(&path, &mut |_| {}).unwrap();
    let map = analyze(&path, &s, &mut |_| {}).unwrap();
    let mut src = open_source(&path, s.mp3.clone().map(Arc::new)).unwrap();
    let ch = src.channels();
    let pcm = decode_all(src.as_mut());

    let hop = map.hop;
    // Hop-aligned ranges: a whole "track", one spanning the quiet gap, and the full file.
    for (a, b) in [(0, 40 * hop), (30 * hop, 70 * hop), (0, map.block_energy.len() as u64 * hop)] {
        let ours = map.range(a, b);
        let mut e = EbuR128::new(ch as u32, RATE, Mode::I | Mode::TRUE_PEAK).unwrap();
        let end = (b as usize * ch).min(pcm.len());
        e.add_frames_f32(&pcm[a as usize * ch..end]).unwrap();
        let direct = e.loudness_global().unwrap();
        let direct_peak = (0..ch as u32).map(|c| e.true_peak(c).unwrap()).fold(0.0, f64::max);
        let direct_peak_db = 20.0 * direct_peak.log10();
        // Only difference: the K-weighting filters were already warmed up by earlier audio.
        assert!((ours.lufs.unwrap() - direct).abs() < 0.2, "[{a}, {b}): {:?} vs {direct}", ours.lufs);
        assert!((ours.peak_db - direct_peak_db).abs() < 0.3, "[{a}, {b}): peak {} vs {direct_peak_db}", ours.peak_db);
    }
}

#[test]
fn normalized_export_hits_the_target_and_is_tagged() {
    use splitter_audio::export::{export, ExportJob, Tags};
    use splitter_audio::loudness::apply_to_jobs;
    use splitter_core::export::{Normalize, Profile};

    let path = tmp("loudness-norm-src.wav");
    write_wav(&path);
    let s = scan(&path, &mut |_| {}).unwrap();
    let map = analyze(&path, &s, &mut |_| {}).unwrap();
    let dir = tmp("loudness-norm");
    // The loud first 5 s, well away from the silent gap.
    let (a, b) = (RATE as u64 / 2, RATE as u64 * 9 / 2);
    let mut jobs = vec![ExportJob {
        start: a,
        end: b,
        path: dir.join("t.flac"),
        gain_db: 0.0,
        tags: Tags { title: "t".into(), track: 1, total: 1, ..Default::default() },
    }];
    let before = map.range(a, b);
    // −14 LUFS would push this signal's peaks past the −1 dBTP ceiling, so aim lower.
    let target = before.lufs.unwrap() as f32 - 3.0;
    apply_to_jobs(&mut jobs, &map, Normalize::Track { target }, true);
    assert!((jobs[0].gain_db + 3.0).abs() < 1e-6, "gain {}", jobs[0].gain_db);
    let rg = jobs[0].tags.replaygain.unwrap();
    assert!((rg.track_gain_db - (-18.0 - target as f64)).abs() < 1e-6, "ReplayGain describes the written level");

    export(&path, &s, &jobs, Profile::Flac, &mut |_| {}).unwrap();
    let out = decode_gapless(&jobs[0].path);
    let mut e = EbuR128::new(2, RATE, Mode::I).unwrap();
    e.add_frames_f32(&out).unwrap();
    let measured = e.loudness_global().unwrap();
    assert!((measured - target as f64).abs() < 0.3, "measured {measured} LUFS, target {target}");
    let bytes = std::fs::read(&jobs[0].path).unwrap();
    let text = String::from_utf8_lossy(&bytes[..4096.min(bytes.len())]);
    assert!(text.contains("REPLAYGAIN_TRACK_GAIN="), "FLAC carries ReplayGain comments");
}

#[test]
fn normalization_never_exceeds_the_peak_ceiling() {
    use splitter_audio::export::{ExportJob, Tags};
    use splitter_audio::loudness::apply_to_jobs;
    use splitter_core::export::Normalize;

    let path = tmp("loudness-ceiling-src.wav");
    write_wav(&path);
    let s = scan(&path, &mut |_| {}).unwrap();
    let map = analyze(&path, &s, &mut |_| {}).unwrap();
    let mut jobs = vec![ExportJob {
        start: 0,
        end: RATE as u64 * 4,
        path: tmp("unused.flac"),
        gain_db: 0.0,
        tags: Tags::default(),
    }];
    apply_to_jobs(&mut jobs, &map, Normalize::Track { target: 0.0 }, true);
    let peak_after = map.range(0, RATE as u64 * 4).peak_db + jobs[0].gain_db;
    assert!(peak_after <= -1.0 + 1e-9, "peak would be {peak_after} dBTP");
    // A byte copy can't change level: no gain, but still tagged.
    apply_to_jobs(&mut jobs, &map, Normalize::Track { target: 0.0 }, false);
    assert_eq!(jobs[0].gain_db, 0.0);
    assert!(jobs[0].tags.replaygain.is_some());
}
