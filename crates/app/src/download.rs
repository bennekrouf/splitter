//! Open a recording from a URL (YouTube or anything else yt-dlp supports): yt-dlp downloads
//! the best audio and ffmpeg converts it to WAV in the downloads folder, which then opens
//! with the new file selected.
//!
//! WAV keeps the rest of the app unchanged: it decodes and seeks exactly, and exporting as
//! "Original" doesn't add a second lossy encode on top of YouTube's.

use crate::state::App;
use crossbeam_channel::{Receiver, Sender};
use dioxus::prelude::*;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Markers we ask yt-dlp to print, so its other output can't be mistaken for ours.
const PROGRESS: &str = "SPLITTER-PROGRESS";
const FILE: &str = "SPLITTER-FILE";

/// Where downloads go: ~/Music/Splitter/Downloads.
pub fn downloads_dir() -> PathBuf {
    dirs::audio_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("Splitter").join("Downloads")
}

enum Event {
    /// Fraction downloaded, when the size is known.
    Progress(Option<f32>),
    /// Download finished; ffmpeg is extracting the audio.
    Converting,
    Done(PathBuf),
    Failed(String),
}

/// What the sidebar shows while a download runs.
#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    Starting,
    Downloading(Option<f32>),
    Converting,
}

/// A running download: yt-dlp in a child process, read on a worker thread.
pub struct Download {
    events: Receiver<Event>,
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
    pub phase: Phase,
}

impl Download {
    fn start(url: &str, dir: &Path) -> Result<Self, String> {
        let yt_dlp = find_tool("yt-dlp").ok_or_else(|| missing("yt-dlp"))?;
        find_tool("ffmpeg").ok_or_else(|| missing("ffmpeg"))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;

        let mut child = Command::new(yt_dlp)
            // Apps started from the Finder don't get the shell's PATH; yt-dlp needs ffmpeg on it.
            .env("PATH", search_path())
            .args(["--no-playlist", "--newline", "--progress", "--no-colors"])
            .args(["-f", "bestaudio/best", "-x", "--audio-format", "wav"])
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

        let (tx, events) = crossbeam_channel::unbounded();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let child = Arc::new(Mutex::new(Some(child)));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (c, k) = (child.clone(), cancelled.clone());
        std::thread::Builder::new()
            .name("splitter-download".into())
            .spawn(move || run(stdout, stderr, &c, &k, &tx))
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
    stdout: impl Read + Send + 'static,
    stderr: impl Read + Send + 'static,
    child: &Mutex<Option<Child>>,
    cancelled: &AtomicBool,
    tx: &Sender<Event>,
) {
    // Progress can come on either stream; errors come on stderr.
    let etx = tx.clone();
    let errors = std::thread::spawn(move || {
        let mut errors = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !parse_line(&line, &etx) && line.starts_with("ERROR:") {
                errors.push(line.trim_start_matches("ERROR:").trim().to_owned());
            }
        }
        errors
    });
    let mut file = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(path) = line.strip_prefix(FILE).map(str::trim) {
            file = Some(PathBuf::from(path));
        } else {
            parse_line(&line, tx);
        }
    }
    let errors = errors.join().unwrap_or_default();
    let status = child.lock().unwrap().take().and_then(|mut c| c.wait().ok());
    if cancelled.load(Ordering::SeqCst) {
        return;
    }
    let event = match (file, status) {
        (Some(path), Some(s)) if s.success() && path.is_file() => Event::Done(path),
        _ if !errors.is_empty() => Event::Failed(explain(&errors.join("\n"))),
        (_, Some(s)) => Event::Failed(format!("yt-dlp stopped ({s}) without producing a file")),
        (_, None) => Event::Failed("yt-dlp stopped without producing a file".into()),
    };
    let _ = tx.send(event);
}

/// Turn one of our progress lines into an event. Returns whether it was one.
fn parse_line(line: &str, tx: &Sender<Event>) -> bool {
    let Some(rest) = line.strip_prefix(PROGRESS) else { return false };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let num = |i: usize| fields.get(i).and_then(|s| s.parse::<f64>().ok());
    let event = match fields.first() {
        Some(&"finished") => Event::Converting,
        _ => {
            let total = num(2).or(num(3)).filter(|&t| t > 0.0);
            Event::Progress(num(1).zip(total).map(|(d, t)| (d / t).clamp(0.0, 1.0) as f32))
        }
    };
    let _ = tx.send(event);
    true
}

/// Add a hint to the errors people will actually hit.
fn explain(err: &str) -> String {
    let hint = if err.contains("Sign in to confirm") || err.contains("not a bot") {
        " — YouTube wants a signed-in session; updating yt-dlp (`brew upgrade yt-dlp`) usually fixes this"
    } else if err.contains("HTTP Error 403") || err.contains("nsig") || err.contains("Requested format") {
        " — yt-dlp is probably out of date: `brew upgrade yt-dlp`"
    } else {
        ""
    };
    format!("Download failed: {err}{hint}")
}

fn missing(tool: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("{tool} is not installed. Install it with: brew install yt-dlp ffmpeg")
    } else {
        format!("{tool} is not installed (it must be on the PATH to open URLs)")
    }
}

/// PATH plus the usual Homebrew locations.
fn search_path() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let extra = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).chain(extra).collect();
    std::env::join_paths(dirs).unwrap_or(path)
}

fn find_tool(name: &str) -> Option<PathBuf> {
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&search_path()).map(|d| d.join(&exe)).find(|p| p.is_file())
}

impl App {
    /// ⌘U: ask for a URL.
    pub fn ask_url(self) {
        let mut dialog = self.url_dialog;
        if self.download.peek().is_none() {
            dialog.set(Some(String::new()));
        }
    }

    pub fn start_download(mut self, url: &str) {
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            self.error.set(Some("That doesn't look like a link (it should start with https://)".into()));
            return;
        }
        match Download::start(url, &downloads_dir()) {
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
                Event::Progress(p) => Phase::Downloading(p),
                Event::Converting => Phase::Converting,
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

    fn parse(line: &str) -> Option<Event> {
        let (tx, rx) = crossbeam_channel::unbounded();
        parse_line(line, &tx);
        rx.try_recv().ok()
    }

    #[test]
    fn progress_lines() {
        match parse("SPLITTER-PROGRESS downloading 250 1000 NA") {
            Some(Event::Progress(Some(p))) => assert!((p - 0.25).abs() < 1e-6),
            _ => panic!("expected 25%"),
        }
        // Only an estimate of the size.
        match parse("SPLITTER-PROGRESS downloading 500 NA 1000.5") {
            Some(Event::Progress(Some(p))) => assert!((p - 0.5).abs() < 1e-3),
            _ => panic!("expected 50%"),
        }
        assert!(matches!(parse("SPLITTER-PROGRESS downloading 500 NA NA"), Some(Event::Progress(None))));
        assert!(matches!(parse("SPLITTER-PROGRESS finished 1000 1000 NA"), Some(Event::Converting)));
        assert!(parse("[youtube] abc: Downloading webpage").is_none());
    }

    /// Needs network, yt-dlp and ffmpeg: `cargo test -p splitter -- --ignored download`
    #[test]
    #[ignore]
    fn downloads_a_short_video() {
        let dir = std::env::temp_dir().join("splitter-download-test");
        let _ = std::fs::remove_dir_all(&dir);
        let d = Download::start("https://www.youtube.com/watch?v=jNQXAC9IVRw", &dir).unwrap();
        let mut progressed = false;
        loop {
            match d.events.recv_timeout(std::time::Duration::from_secs(120)).expect("timed out") {
                Event::Progress(Some(_)) => progressed = true,
                Event::Progress(None) | Event::Converting => {}
                Event::Done(path) => {
                    assert!(progressed);
                    assert_eq!(path.extension().unwrap(), "wav");
                    assert_eq!(path.parent().unwrap(), dir);
                    assert!(splitter_core::is_audio(&path));
                    break;
                }
                Event::Failed(e) => panic!("{e}"),
            }
        }
    }
}
