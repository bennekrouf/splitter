//! Export every recording in a folder from its `splitter.cutlist.json`, without the UI, and
//! optionally verify each track decodes (gaplessly) to exactly its planned length.
//!
//!     cargo run -p splitter --release --example export_cutlist -- testdata [--out DIR] [--verify]
//!         [--profile original|mp3-v0|mp3-v2|mp3-v4|mp3-320|mp3-256|mp3-192|mp3-128|flac|wav16]

use splitter_audio::export::{export, ExportJob, Tags};
use splitter_audio::scan::load_or_scan;
use splitter_core::cutlist::Cutlist;
use splitter_core::export::{plan, Profile};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: export_cutlist <folder> [--out DIR] [--verify]"));
    let mut out_root = None;
    let mut verify = false;
    let mut profile_arg: Option<Profile> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out_root = args.next().map(PathBuf::from),
            "--verify" => verify = true,
            "--profile" => {
                let name = args.next().expect("--profile needs a value");
                profile_arg = Some(match name.as_str() {
                    "original" => Profile::Original,
                    "flac" => Profile::Flac,
                    "wav16" => Profile::Wav16,
                    n if n.starts_with("mp3-v") => Profile::Mp3Vbr { quality: n[5..].parse().expect("mp3-vN") },
                    n if n.starts_with("mp3-") => Profile::Mp3Cbr { kbps: n[4..].parse().expect("mp3-KBPS") },
                    n => panic!("unknown profile {n}"),
                });
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let mut cutlist = Cutlist::load(&dir).expect("reading cutlist");
    if let Some(p) = profile_arg {
        cutlist.export.profile = p;
    }
    let profile = cutlist.export.profile;
    println!("profile: {}", profile.label());
    let mut failures = 0;

    for (name, edit) in &cutlist.recordings {
        let path = dir.join(name);
        let scan = load_or_scan(&path, &mut |_| {}).expect("scan");
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let src_ext = path.extension().unwrap().to_string_lossy().to_lowercase();
        let ext = profile.extension(&src_ext);
        let out = out_root.clone().unwrap_or_else(|| dir.clone()).join(&stem);
        let planned = plan(edit, scan.info.total_samples, &stem, &cutlist.export);
        let jobs: Vec<ExportJob> = planned
            .iter()
            .map(|t| ExportJob {
                start: t.start,
                end: t.end,
                path: out.join(format!("{}.{ext}", t.stem)),
                tags: Tags { title: t.title.clone(), album: stem.clone(), track: t.number, total: t.total },
            })
            .collect();

        let t0 = Instant::now();
        export(&path, &scan, &jobs, profile, &mut |_| {}).expect("export");
        let bytes: u64 = jobs.iter().map(|j| std::fs::metadata(&j.path).map(|m| m.len()).unwrap_or(0)).sum();
        println!(
            "{name}: {} tracks in {:.2} s, {:.1} MB → {}",
            jobs.len(),
            t0.elapsed().as_secs_f64(),
            bytes as f64 / 1e6,
            out.display()
        );

        if verify {
            let rate = scan.info.sample_rate as f64;
            for job in &jobs {
                let got = gapless_frames(&job.path);
                // A copied MP3 track starting at 0 can't include the decoder's first 529 samples;
                // re-encoded tracks start exactly.
                let lead = if scan.mp3.is_some() && profile == Profile::Original { 529 } else { 0 };
                let want = job.end - job.start.max(lead);
                let ok = got == want;
                failures += !ok as usize;
                println!(
                    "  {} {:>9.3}s  {}",
                    if ok { "ok  " } else { "FAIL" },
                    got as f64 / rate,
                    job.path.file_name().unwrap().to_string_lossy()
                );
            }
        }
    }
    if failures > 0 {
        eprintln!("{failures} track(s) had the wrong length");
        std::process::exit(1);
    }
}

/// Length in sample frames when decoded with LAME gapless trimming on.
fn gapless_frames(path: &std::path::Path) -> u64 {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    let mss = MediaSourceStream::new(Box::new(std::fs::File::open(path).unwrap()), Default::default());
    let opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let mut format = symphonia::default::get_probe()
        .format(&Default::default(), mss, &opts, &Default::default())
        .unwrap()
        .format;
    let track = format.default_track().unwrap().clone();
    let mut dec = symphonia::default::get_codecs().make(&track.codec_params, &Default::default()).unwrap();
    let mut frames = 0;
    while let Ok(p) = format.next_packet() {
        frames += dec.decode(&p).unwrap().frames() as u64;
    }
    frames
}
