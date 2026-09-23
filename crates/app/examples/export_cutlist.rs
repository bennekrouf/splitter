//! Export every recording in a folder from its `splitter.cutlist.json`, without the UI, and
//! optionally verify each track decodes (gaplessly) to exactly its planned length.
//!
//!     cargo run -p splitter --release --example export_cutlist -- testdata [--out DIR] [--verify]

use splitter_audio::export::{export, ExportJob, Tags};
use splitter_audio::scan::load_or_scan;
use splitter_core::cutlist::Cutlist;
use splitter_core::export::plan;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: export_cutlist <folder> [--out DIR] [--verify]"));
    let mut out_root = None;
    let mut verify = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out_root = args.next().map(PathBuf::from),
            "--verify" => verify = true,
            other => panic!("unknown argument {other}"),
        }
    }
    let cutlist = Cutlist::load(&dir).expect("reading cutlist");
    let mut failures = 0;

    for (name, edit) in &cutlist.recordings {
        let path = dir.join(name);
        let scan = load_or_scan(&path, &mut |_| {}).expect("scan");
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let ext = path.extension().unwrap().to_string_lossy().to_lowercase();
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
        export(&path, &scan, &jobs, &mut |_| {}).expect("export");
        println!("{name}: {} tracks in {:.2} s → {}", jobs.len(), t0.elapsed().as_secs_f64(), out.display());

        if verify {
            let rate = scan.info.sample_rate as f64;
            for job in &jobs {
                let got = gapless_frames(&job.path);
                // A track starting at 0 can't include the decoder's first 529 samples.
                let want = job.end - job.start.max(if scan.mp3.is_some() { 529 } else { 0 });
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
