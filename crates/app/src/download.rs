//! Open a recording from a URL (YouTube or anything else yt-dlp supports): yt-dlp downloads
//! the audio stream, we convert it to WAV in the downloads folder, which then opens with the
//! new file selected. Or, when asked for the video, an MP4 with its sound, kept as is.
//!
//! yt-dlp and Deno are installed and kept up to date by `tools`; nothing else is needed.
//! We ask for AAC (.m4a) or MP3 so no ffmpeg is involved: MP3 is kept as is, and AAC is decoded
//! to WAV with symphonia. WAV keeps the rest of the app unchanged: it decodes and seeks exactly,
//! and exporting as "Original" doesn't add a second lossy encode on top of the site's.
//! For a video, picture (H.264, what every web view plays) and sound (AAC) are downloaded
//! separately, as sites like YouTube serve them, and merged into one MP4 by ffmpeg (installed
//! by `tools` on first use).

use crate::state::App;
use crate::tools;
use crossbeam_channel::{Receiver, Sender};
use dioxus::prelude::*;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Markers we ask yt-dlp to print, so its other output can't be mistaken for ours.
const PROGRESS: &str = "SPLITTER-PROGRESS";
const FILE: &str = "SPLITTER-FILE";

/// Where downloads go: ~/Music/Splitter/Downloads.
pub fn downloads_dir() -> PathBuf {
    dirs::audio_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("Splitter").join("Downloads")
}

/// What to download from a URL.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Kind {
    /// The sound only, as WAV (or MP3).
    #[default]
    Audio,
    /// Picture and sound, as one MP4.
    Video,
}

impl Kind {
    /// yt-dlp format selection: only formats symphonia decodes and the web view plays.
    fn formats(self) -> &'static str {
        match self {
            Kind::Audio => "ba[ext=m4a]/ba[ext=mp3]",
            // Separate picture and sound merged ("+"), or a file that already has both.
            Kind::Video => {
                "bv*[ext=mp4][vcodec^=avc1][height<=1080]+ba[ext=m4a]/b[ext=mp4][vcodec^=avc1][acodec^=mp4a]"
            }
        }
    }
}

enum Event {
    Phase(Phase),
    Done(PathBuf),
    Failed(String),
}

/// What the sidebar shows while a download runs. Fractions are `None` when unknown.
#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    /// First use: fetching yt-dlp and Deno.
    Installing(f32),
    Starting,
    Downloading(Option<f32>),
    Converting(Option<f32>),
}

/// A running download, on a worker thread.
pub struct Download {
    events: Receiver<Event>,
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
    pub phase: Phase,
}

impl Download {
    fn start(url: &str, kind: Kind, dir: &Path, tools_dir: &Path) -> Result<Self, String> {
        let (tx, events) = crossbeam_channel::unbounded();
        let child = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (c, k) = (child.clone(), cancelled.clone());
        let (url, dir, tools_dir) = (url.to_owned(), dir.to_owned(), tools_dir.to_owned());
        std::thread::Builder::new()
            .name("splitter-download".into())
            .spawn(move || {
                let result = run(&url, kind, &dir, &tools_dir, &c, &k, &tx);
                if !k.load(Ordering::SeqCst) {
                    let _ = tx.send(match result {
                        Ok(path) => Event::Done(path),
                        Err(e) => Event::Failed(e),
                    });
                }
            })
            .map_err(|e| format!("Could not start the download: {e}"))?;
        Ok(Self { events, child, cancelled, phase: Phase::Starting })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
}

fn run(
    url: &str,
    kind: Kind,
    dir: &Path,
    tools_dir: &Path,
    child: &Mutex<Option<Child>>,
    cancelled: &AtomicBool,
    tx: &Sender<Event>,
) -> Result<PathBuf, String> {
    let phase = |p: Phase| {
        let _ = tx.send(Event::Phase(p));
    };
    let is_cancelled = || cancelled.load(Ordering::SeqCst);
    let tools = tools::ensure(tools_dir, &mut |p| phase(Phase::Installing(p)), &is_cancelled)?;
    let ffmpeg = match kind {
        Kind::Video => Some(tools::ensure_ffmpeg(tools_dir, &mut |p| phase(Phase::Installing(p)), &is_cancelled)?),
        Kind::Audio => None,
    };
    phase(Phase::Starting);
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;

    let mut cmd = tools::command(&tools.yt_dlp);
    cmd.arg("--ignore-config")
        .args(["--no-playlist", "--newline", "--progress", "--no-colors"])
        .arg("--js-runtimes")
        .arg(format!("deno:{}", tools.deno.display()))
        // Formats symphonia decodes (and, for a video, the web view plays).
        .args(["-f", kind.formats()]);
    match &ffmpeg {
        Some(ffmpeg) => cmd.arg("--ffmpeg-location").arg(ffmpeg).args(["--merge-output-format", "mp4"]),
        // No ffmpeg to convert or fix up anything.
        None => cmd.args(["--fixup", "never"]),
    };
    let mut spawned = cmd
        .args([
            "--progress-template",
            &format!(
                "download:{PROGRESS} %(progress.status)s %(progress.downloaded_bytes)s \
                 %(progress.total_bytes)s %(progress.total_bytes_estimate)s"
            ),
        ])
        .args(["--print", &format!("after_move:{FILE} %(filepath)s")])
        .arg("-P")
        .arg(dir)
        .args(["-o", "%(title).150B [%(id)s].%(ext)s"])
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start yt-dlp: {e}"))?;
    let stdout = spawned.stdout.take().unwrap();
    let stderr = spawned.stderr.take().unwrap();
    *child.lock().unwrap() = Some(spawned);
    if is_cancelled() {
        // Cancelled while starting: `cancel` found no child to kill.
        if let Some(c) = child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
    }

    let (files, errors) = read_output(stdout, stderr, tx);
    let status = child.lock().unwrap().take().and_then(|mut c| c.wait().ok());
    if is_cancelled() {
        return Err("cancelled".into());
    }
    let file = match (files.last(), status) {
        (Some(path), Some(s)) if s.success() && path.is_file() => path.clone(),
        _ if !errors.is_empty() => return Err(explain(&errors.join("\n"), kind)),
        (_, Some(s)) => return Err(format!("yt-dlp stopped ({s}) without producing a file")),
        (_, None) => return Err("yt-dlp stopped without producing a file".into()),
    };
    if kind == Kind::Video || !file.extension().is_some_and(|e| e.eq_ignore_ascii_case("m4a")) {
        return Ok(file); // MP3 or a video: the app plays and exports it directly
    }
    phase(Phase::Converting(None));
    let wav = file.with_extension("wav");
    splitter_audio::transcode::decode_to_wav(&file, &wav, &mut |p| phase(Phase::Converting(Some(p))))
        .map_err(|e| format!("Could not convert the download to WAV: {e:#}"))?;
    let _ = std::fs::remove_file(&file);
    Ok(wav)
}

/// Forward progress; returns the downloaded files, in order, and yt-dlp's error lines.
fn read_output(
    stdout: impl Read + Send + 'static,
    stderr: impl Read + Send + 'static,
    tx: &Sender<Event>,
) -> (Vec<PathBuf>, Vec<String>) {
    // Progress can come on either stream; errors come on stderr.
    let etx = tx.clone();
    let errors = std::thread::spawn(move || {
        let mut errors = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(p) = parse_progress(&line) {
                let _ = etx.send(Event::Phase(p));
            } else if let Some(e) = line.strip_prefix("ERROR:") {
                errors.push(e.trim().to_owned());
            }
        }
        errors
    });
    let mut files = Vec::new();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(path) = line.strip_prefix(FILE).map(str::trim) {
            files.push(PathBuf::from(path));
        } else if let Some(p) = parse_progress(&line) {
            let _ = tx.send(Event::Phase(p));
        }
    }
    (files, errors.join().unwrap_or_default())
}

/// One of our progress lines, as a phase.
fn parse_progress(line: &str) -> Option<Phase> {
    let fields: Vec<&str> = line.strip_prefix(PROGRESS)?.split_whitespace().collect();
    let num = |i: usize| fields.get(i).and_then(|s| s.parse::<f64>().ok());
    if fields.first() == Some(&"finished") {
        return Some(Phase::Converting(None));
    }
    let total = num(2).or(num(3)).filter(|&t| t > 0.0);
    Some(Phase::Downloading(num(1).zip(total).map(|(d, t)| (d / t).clamp(0.0, 1.0) as f32)))
}

/// Add a hint to the errors people will actually hit.
fn explain(err: &str, kind: Kind) -> String {
    let hint = if err.contains("Requested format is not available") {
        match kind {
            Kind::Audio => " — this site offers no M4A or MP3 audio for it",
            Kind::Video => " — this site offers no H.264 MP4 video for it; try Audio",
        }
    } else if err.contains("Sign in to confirm") || err.contains("not a bot") || err.contains("HTTP Error 403") {
        " — YouTube is refusing this network for now; yt-dlp updates itself daily, so try again later"
    } else {
        ""
    };
    format!("Download failed: {err}{hint}")
}

impl App {
    /// ⌘U: ask for a URL.
    pub fn ask_url(self) {
        let mut dialog = self.url_dialog;
        if self.download.peek().is_none() {
            dialog.set(Some(String::new()));
        }
    }

    pub fn start_download(mut self, url: &str, kind: Kind) {
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            self.error.set(Some("That doesn't look like a link (it should start with https://)".into()));
            return;
        }
        match Download::start(url, kind, &downloads_dir(), &tools::dir()) {
            Ok(d) => self.download.set(Some(d)),
            Err(e) => self.error.set(Some(e)),
        }
    }

    pub fn cancel_download(mut self) {
        if let Some(d) = self.download.peek().as_ref() {
            d.cancel();
        }
        self.download.set(None);
    }

    /// Called every frame.
    pub fn poll_download(mut self) {
        loop {
            let event = match self.download.peek().as_ref() {
                Some(d) => d.events.try_recv(),
                None => return,
            };
            let Ok(event) = event else { return };
            let phase = match event {
                Event::Phase(p) => p,
                Event::Done(path) => {
                    self.download.set(None);
                    self.open(&path);
                    return;
                }
                Event::Failed(e) => {
                    self.download.set(None);
                    self.error.set(Some(e));
                    return;
                }
            };
            if self.download.peek().as_ref().is_some_and(|d| d.phase != phase) {
                if let Some(d) = self.download.write().as_mut() {
                    d.phase = phase;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines() {
        let frac = |line| match parse_progress(line) {
            Some(Phase::Downloading(p)) => p,
            _ => panic!("not a download line: {line}"),
        };
        assert!((frac("SPLITTER-PROGRESS downloading 250 1000 NA").unwrap() - 0.25).abs() < 1e-6);
        // Only an estimate of the size.
        assert!((frac("SPLITTER-PROGRESS downloading 500 NA 1000.5").unwrap() - 0.5).abs() < 1e-3);
        assert_eq!(frac("SPLITTER-PROGRESS downloading 500 NA NA"), None);
        assert!(matches!(parse_progress("SPLITTER-PROGRESS finished 1000 1000 NA"), Some(Phase::Converting(None))));
        assert!(parse_progress("[youtube] abc: Downloading webpage").is_none());
    }

    /// Needs the network; installs the tools into the app's real tools folder on first run.
    /// `cargo test -p splitter -- --ignored download`
    #[test]
    #[ignore]
    fn downloads_a_short_video() {
        let dir = std::env::temp_dir().join("splitter-download-test");
        let _ = std::fs::remove_dir_all(&dir);
        let d =
            Download::start("https://www.youtube.com/watch?v=jNQXAC9IVRw", Kind::Audio, &dir, &tools::dir()).unwrap();
        let mut downloaded = false;
        loop {
            match d.events.recv_timeout(std::time::Duration::from_secs(300)).expect("timed out") {
                Event::Phase(Phase::Downloading(Some(_))) => downloaded = true,
                Event::Phase(_) => {}
                Event::Done(path) => {
                    assert!(downloaded);
                    assert_eq!(path.extension().unwrap(), "wav");
                    assert_eq!(path.parent().unwrap(), dir);
                    // 19 s of 44.1 kHz stereo 16-bit, and the .m4a is gone.
                    let len = std::fs::metadata(&path).unwrap().len();
                    assert!((3_000_000..3_600_000).contains(&len), "{len} bytes");
                    assert!(!path.with_extension("m4a").exists());
                    break;
                }
                Event::Failed(e) => panic!("{e}"),
            }
        }
    }

    /// Needs the network, like the audio one.
    /// `cargo test -p splitter -- --ignored download`
    #[test]
    #[ignore]
    fn downloads_a_short_video_as_one_mp4() {
        let dir = std::env::temp_dir().join("splitter-download-video-test");
        let _ = std::fs::remove_dir_all(&dir);
        let d =
            Download::start("https://www.youtube.com/watch?v=jNQXAC9IVRw", Kind::Video, &dir, &tools::dir()).unwrap();
        loop {
            match d.events.recv_timeout(std::time::Duration::from_secs(300)).expect("timed out") {
                Event::Phase(_) => {}
                Event::Done(path) => {
                    // One file with picture and sound, whose sound opens like any recording's.
                    assert_eq!(path.extension().unwrap(), "mp4");
                    let scan = splitter_audio::scan::scan(&path, &mut |_| {}).unwrap();
                    assert_eq!(scan.info.codec, "AAC");
                    assert!((18.0..20.0).contains(&scan.info.duration_secs()), "{}", scan.info.duration_secs());
                    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "leftover parts");
                    break;
                }
                Event::Failed(e) => panic!("{e}"),
            }
        }
    }
}
