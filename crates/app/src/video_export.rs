//! Video clips with ffmpeg: each track of a video cut out on its exact start and end and
//! re-encoded (H.264 + AAC), and the cleaned copy of "Apply cuts" for a video.
//!
//! Positions come in as our sample frames and go to ffmpeg as seconds on the video's own
//! timeline, which skips the AAC priming we decode (`splitter_audio::mp4`).

use splitter_audio::export::Tags;
use splitter_core::export::VideoProfile;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Converts our sample frames into seconds on a video's timeline.
#[derive(Clone, Copy, Debug)]
pub struct Timeline {
    pub rate: u32,
    /// Seconds of AAC priming at the start of our decode, which the video's timeline skips.
    pub delay: f64,
}

impl Timeline {
    pub fn of(path: &Path, rate: u32) -> Self {
        Self { rate, delay: splitter_audio::mp4::audio_delay_secs(path).unwrap_or(0.0) }
    }

    pub fn secs(&self, frame: u64) -> f64 {
        (frame as f64 / self.rate as f64 - self.delay).max(0.0)
    }

    /// Our sample frame at `secs` on the video's timeline.
    pub fn frame(&self, secs: f64) -> u64 {
        ((secs + self.delay) * self.rate as f64).round() as u64
    }
}

/// One clip to write: `[start, end)` in sample frames.
pub struct Clip<'a> {
    pub start: u64,
    pub end: u64,
    pub path: &'a Path,
    pub tags: &'a Tags,
    /// Level change in dB (loudness normalization).
    pub gain_db: f64,
}

/// Encoding settings shared by clips and cleaned copies.
fn encode_args(cmd: &mut Command, profile: VideoProfile) {
    let VideoProfile::Mp4 { crf } = profile;
    cmd.args(["-c:v", "libx264", "-preset", "medium", "-crf", &crf.to_string(), "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", &format!("{}k", profile.audio_kbps())])
        .args(["-movflags", "+faststart"]);
}

/// Cut `clip` out of `source` into its own MP4.
pub fn export_clip(
    ffmpeg: &Path,
    source: &Path,
    timeline: Timeline,
    clip: &Clip,
    profile: VideoProfile,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(f32),
) -> Result<(), String> {
    let (start, end) = (timeline.secs(clip.start), timeline.secs(clip.end));
    let mut cmd = crate::tools::command(ffmpeg);
    // Seeking before the input while re-encoding is exact: ffmpeg decodes from the keyframe
    // before and drops what comes ahead of `start`.
    cmd.args(["-hide_banner", "-nostdin", "-y", "-loglevel", "error"])
        .args(["-ss", &format!("{start:.6}"), "-i"])
        .arg(source)
        .args(["-t", &format!("{:.6}", end - start)])
        .args(["-map", "0:v:0", "-map", "0:a:0", "-map_metadata", "-1", "-sn", "-dn"]);
    encode_args(&mut cmd, profile);
    if clip.gain_db.abs() > 0.005 {
        cmd.args(["-af", &format!("volume={:.2}dB", clip.gain_db)]);
    }
    let tags = clip.tags;
    cmd.args(["-metadata", &format!("title={}", tags.title)])
        .args(["-metadata", &format!("album={}", tags.album)])
        .args(["-metadata", &format!("track={}/{}", tags.track, tags.total)]);
    run(cmd, clip.path, end - start, cancelled, progress)
}

/// Write the `ranges` (sample frames, sorted) of `source` back to back into a new MP4
/// (Apply cuts, which runs to the end once started).
pub fn write_ranges(
    ffmpeg: &Path,
    source: &Path,
    timeline: Timeline,
    ranges: &[(u64, u64)],
    dst: &Path,
    profile: VideoProfile,
    progress: &mut dyn FnMut(f32),
) -> Result<(), String> {
    let secs: Vec<(f64, f64)> = ranges.iter().map(|&(a, b)| (timeline.secs(a), timeline.secs(b))).collect();
    let total: f64 = secs.iter().map(|(a, b)| b - a).sum();
    let mut cmd = crate::tools::command(ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-i"]).arg(source).args([
        "-filter_complex",
        &concat_filter(&secs),
        "-map",
        "[v]",
        "-map",
        "[a]",
        "-map_metadata",
        "-1",
    ]);
    encode_args(&mut cmd, profile);
    run(cmd, dst, total, &|| false, progress)
}

/// Each `[start, end)` of picture and sound, trimmed and joined.
fn concat_filter(ranges: &[(f64, f64)]) -> String {
    let n = ranges.len();
    let labels = |p: &str| (0..n).map(|i| format!("[{p}{i}]")).collect::<String>();
    let mut f = format!("[0:v:0]split={n}{};[0:a:0]asplit={n}{};", labels("sv"), labels("sa"));
    for (i, (a, b)) in ranges.iter().enumerate() {
        f += &format!("[sv{i}]trim=start={a:.6}:end={b:.6},setpts=PTS-STARTPTS[v{i}];");
        f += &format!("[sa{i}]atrim=start={a:.6}:end={b:.6},asetpts=PTS-STARTPTS[a{i}];");
    }
    f += &(0..n).map(|i| format!("[v{i}][a{i}]")).collect::<String>();
    f += &format!("concat=n={n}:v=1:a=1[v][a]");
    f
}

/// Run ffmpeg writing to a temporary name next to `out`, then move it into place. Reports
/// progress through `-progress`, and stops (removing the partial file) when cancelled.
fn run(
    mut cmd: Command,
    out: &Path,
    secs: f64,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(f32),
) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    let tmp = PathBuf::from(format!("{}.part", out.display()));
    cmd.args(["-progress", "pipe:1", "-nostats", "-f", "mp4"])
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child: Child = cmd.spawn().map_err(|e| format!("Could not start ffmpeg: {e}"))?;
    let mut stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut stopped = false;
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if cancelled() {
            let _ = child.kill();
            stopped = true;
            break;
        }
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
            if secs > 0.0 {
                progress((us / 1e6 / secs).clamp(0.0, 1.0) as f32);
            }
        }
    }
    let status = child.wait().map_err(|e| format!("ffmpeg: {e}"));
    let errors = errors.join().unwrap_or_default();
    let result = match status {
        _ if stopped => Err("cancelled".to_string()),
        Ok(s) if s.success() => {
            std::fs::rename(&tmp, out).map_err(|e| format!("Could not write {}: {e}", out.display()))
        }
        Ok(s) => Err(format!("ffmpeg failed ({s}): {}", errors.trim().lines().last().unwrap_or("no details"))),
        Err(e) => Err(e),
    };
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_skips_the_priming() {
        let t = Timeline { rate: 44100, delay: 1024.0 / 44100.0 };
        assert!((t.secs(44100 + 1024) - 1.0).abs() < 1e-9);
        assert_eq!(t.secs(0), 0.0);
        assert_eq!(t.frame(1.0), 44100 + 1024);
    }

    #[test]
    fn concat_filter_joins_every_range() {
        let f = concat_filter(&[(0.0, 1.5), (3.0, 4.25)]);
        assert!(f.starts_with("[0:v:0]split=2[sv0][sv1];[0:a:0]asplit=2[sa0][sa1];"), "{f}");
        assert!(f.contains("[sv1]trim=start=3.000000:end=4.250000,setpts=PTS-STARTPTS[v1];"), "{f}");
        assert!(f.ends_with("[v0][a0][v1][a1]concat=n=2:v=1:a=1[v][a]"), "{f}");
    }

    /// Exports two clips from the fixture with the installed ffmpeg and checks that each one
    /// has exactly the source's sound for its range. Skipped when ffmpeg isn't installed
    /// (`cargo test -p splitter -- --ignored ffmpeg` installs it).
    #[test]
    fn clips_are_cut_exactly() {
        let ffmpeg = crate::tools::dir().join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX));
        if !ffmpeg.is_file() {
            eprintln!("skipped: no ffmpeg in {}", crate::tools::dir().display());
            return;
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../audio/tests/fixtures/video.mp4");
        let scan = splitter_audio::scan::scan(&source, &mut |_| {}).unwrap();
        let timeline = Timeline::of(&source, scan.info.sample_rate);
        let dir = std::env::temp_dir().join("splitter-video-export-test");
        let _ = std::fs::remove_dir_all(&dir);
        let tags = Tags { title: "Été".into(), album: "Live".into(), track: 1, total: 2, replaygain: None };
        let rate = scan.info.sample_rate as u64;
        // The fixture's signal is loud except for a quiet gap at 5–6 s.
        for (name, start, end) in [("a.mp4", rate / 2, 4 * rate + 12_345), ("b.mp4", 7 * rate, 11 * rate)] {
            let path = dir.join(name);
            let clip = Clip { start: start + 1024, end: end + 1024, path: &path, tags: &tags, gain_db: 0.0 };
            export_clip(&ffmpeg, &source, timeline, &clip, VideoProfile::default(), &|| false, &mut |_| {}).unwrap();
            let got = decode_with(&ffmpeg, &path);
            let all = decode_with(&ffmpeg, &source);
            let want = &all[start as usize..end as usize];
            assert!(
                (got.len() as i64 - want.len() as i64).abs() <= 2 * 1024,
                "{name}: {} samples, want {}",
                got.len(),
                want.len()
            );
            assert_eq!(best_lag(&got, want, 256), 0, "{name} is shifted");
        }
    }

    /// Apply cuts on a video: two ranges joined, each landing where it should.
    #[test]
    fn cleaned_copy_joins_the_kept_ranges() {
        let ffmpeg = crate::tools::dir().join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX));
        if !ffmpeg.is_file() {
            eprintln!("skipped: no ffmpeg in {}", crate::tools::dir().display());
            return;
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../audio/tests/fixtures/video.mp4");
        let rate = 44100u64;
        let timeline = Timeline::of(&source, rate as u32);
        let dst = std::env::temp_dir().join("splitter-video-cleaned-test.mp4");
        let (a, b) = ((rate, 4 * rate), (7 * rate, 10 * rate));
        let ranges = [(a.0 + 1024, a.1 + 1024), (b.0 + 1024, b.1 + 1024)];
        write_ranges(&ffmpeg, &source, timeline, &ranges, &dst, VideoProfile::default(), &mut |_| {}).unwrap();

        let all = decode_with(&ffmpeg, &source);
        let got = decode_with(&ffmpeg, &dst);
        let first = (a.1 - a.0) as usize;
        assert!((got.len() as i64 - (first + (b.1 - b.0) as usize) as i64).abs() <= 2048, "{} samples", got.len());
        assert_eq!(best_lag(&got[..first], &all[a.0 as usize..a.1 as usize], 256), 0, "first range shifted");
        assert_eq!(best_lag(&got[first..], &all[b.0 as usize..b.1 as usize], 256), 0, "second range shifted");
        // The copy has its own priming, which carried-over splits are moved by.
        assert_eq!(Timeline::of(&dst, rate as u32).frame(0.0), 1024);
    }

    /// Mono samples of a file as ffmpeg decodes it (edit list applied).
    fn decode_with(ffmpeg: &Path, path: &Path) -> Vec<f32> {
        let out = crate::tools::command(ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-vn", "-ac", "1", "-f", "f32le", "-"])
            .output()
            .unwrap();
        out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
    }

    /// The shift of `got` against `want` (within ±`max`) with the best match.
    fn best_lag(got: &[f32], want: &[f32], max: i64) -> i64 {
        let n = got.len().min(want.len()) as i64 - 2 * max;
        (-max..=max)
            .max_by(|&a, &b| {
                let score = |lag: i64| -> f64 {
                    (max..max + n).map(|i| got[i as usize] as f64 * want[(i + lag) as usize] as f64).sum()
                };
                score(a).total_cmp(&score(b))
            })
            .unwrap()
    }
}
