//! Time the scan and random seeks on a real file.
//!
//!     cargo run -p splitter-audio --release --example bench -- path/to/recording.mp3

use splitter_audio::scan::scan;
use splitter_audio::source::open_source;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: bench <file>"));

    let t = Instant::now();
    let s = scan(&path, &mut |_| {}).expect("scan");
    let secs = s.info.duration_secs();
    println!(
        "scan: {:.2} s for {:.1} min of audio ({:.0}x realtime), {} frames indexed",
        t.elapsed().as_secs_f64(),
        secs / 60.0,
        secs / t.elapsed().as_secs_f64(),
        s.mp3.as_ref().map(|m| m.frames()).unwrap_or(0)
    );

    let mp3 = s.mp3.clone().map(Arc::new);
    let mut src = open_source(&path, mp3).expect("open");
    let total = s.info.total_samples;
    let mut worst = 0.0f64;
    let mut sum = 0.0;
    let n = 200;
    let mut x: u64 = 88172645463325252;
    let mut buf = Vec::new();
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let target = x % total;
        let t = Instant::now();
        src.seek(target).unwrap();
        buf.clear();
        src.read(&mut buf).unwrap(); // first audible chunk
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst = worst.max(ms);
        sum += ms;
    }
    println!("seek + first chunk: avg {:.2} ms, worst {:.2} ms over {n} random seeks", sum / n as f64, worst);
}
