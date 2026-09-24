//! App state and the actions the UI and keyboard trigger.

use crate::exporting::{ExportStatus, Exporter};
use dioxus::prelude::*;
use splitter_audio::loudness::LoudnessMap;
use splitter_audio::{Player, Scan, ScanEvent, Scanner};
use splitter_core::cutlist::Cutlist;
use splitter_core::edit::{History, Snapshot};
use splitter_core::{list_recordings, Recording, Status};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub enum ScanState {
    Scanning(f32),
    Ready(Arc<Scan>),
    Failed(String),
}

/// Detail view window: start sample and width in seconds.
#[derive(Clone, Copy, PartialEq)]
pub struct View {
    pub start: u64,
    pub span_secs: f64,
}

impl View {
    pub const MIN_SPAN: f64 = 2.0;
    pub const MAX_SPAN: f64 = 600.0;
}

/// All reactive state. Signals are `Copy`, so this is passed around by value.
#[derive(Clone, Copy)]
pub struct App {
    pub folder: Signal<Option<PathBuf>>,
    pub recordings: Signal<Vec<Recording>>,
    pub selected: Signal<Option<usize>>,
    pub scans: Signal<HashMap<PathBuf, ScanState>>,
    /// Playhead in source samples; only leaf components should read this (it changes at 60 fps).
    pub pos: Signal<u64>,
    pub playing: Signal<bool>,
    pub view: Signal<View>,
    pub error: Signal<Option<String>>,
    pub root: Signal<Option<Rc<MountedData>>>,
    pub player: Signal<Player>,
    pub scanner: Signal<Scanner>,
    /// Edits for every recording in the folder; saved as `splitter.cutlist.json`.
    pub cutlist: Signal<Cutlist>,
    /// Unsaved changes since this instant (debounced saves during drags).
    pub cut_dirty: Signal<Option<Instant>>,
    pub history: Signal<HashMap<String, History>>,
    /// Index of the split being reviewed in the current recording.
    pub cur_split: Signal<Option<usize>>,
    /// The edit as it was when a marker drag started (for a single undo step).
    pub drag: Signal<Option<Snapshot>>,
    /// Track whose title is being typed.
    pub editing_title: Signal<Option<usize>>,
    /// Text of the "paste tracklist" dialog while it's open.
    pub paste: Signal<Option<String>>,
    pub exporter: Signal<Exporter>,
    /// Export queue state per recording (by cutlist key).
    pub exports: Signal<HashMap<String, ExportStatus>>,
    /// Loudness measurements per recording, as they arrive from the background worker.
    pub loudness: Signal<HashMap<PathBuf, Arc<LoudnessMap>>>,
    pub ab: Signal<crate::ab::AbState>,
    pub ab_rx: Signal<Option<crossbeam_channel::Receiver<crate::ab::AbResult>>>,
    /// When the current error message was first shown (they fade after a while).
    error_since: Signal<Option<Instant>>,
}

/// How long an error message stays up.
const ERROR_SECS: u64 = 10;

impl App {
    pub fn new() -> Self {
        Self {
            folder: Signal::new(None),
            recordings: Signal::new(Vec::new()),
            selected: Signal::new(None),
            scans: Signal::new(HashMap::new()),
            pos: Signal::new(0),
            playing: Signal::new(false),
            view: Signal::new(View { start: 0, span_secs: 20.0 }),
            error: Signal::new(None),
            root: Signal::new(None),
            player: Signal::new(Player::spawn()),
            scanner: Signal::new(Scanner::spawn()),
            cutlist: Signal::new(Cutlist::default()),
            cut_dirty: Signal::new(None),
            history: Signal::new(HashMap::new()),
            cur_split: Signal::new(None),
            drag: Signal::new(None),
            editing_title: Signal::new(None),
            paste: Signal::new(None),
            exporter: Signal::new(Exporter::spawn()),
            exports: Signal::new(HashMap::new()),
            loudness: Signal::new(HashMap::new()),
            ab: Signal::new(Default::default()),
            ab_rx: Signal::new(None),
            error_since: Signal::new(None),
        }
    }

    pub(crate) fn player(&self) -> Player {
        self.player.peek().clone()
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        let i = (*self.selected.peek())?;
        self.recordings.peek().get(i).map(|r| r.path.clone())
    }

    /// The selected recording's scan, without subscribing.
    pub fn current_scan(&self) -> Option<Arc<Scan>> {
        let path = self.selected_path()?;
        match self.scans.peek().get(&path) {
            Some(ScanState::Ready(s)) => Some(s.clone()),
            _ => None,
        }
    }

    /// Open a folder, or a file's folder with that file selected.
    pub fn open(mut self, path: &Path) {
        let (dir, file) = if path.is_dir() {
            (path.to_owned(), None)
        } else {
            (path.parent().unwrap_or(Path::new(".")).to_owned(), Some(path.to_owned()))
        };
        let recordings = match list_recordings(&dir) {
            Ok(r) => r,
            Err(e) => {
                self.error.set(Some(format!("Could not read {}: {e}", dir.display())));
                return;
            }
        };
        self.save_now();
        let cutlist = match Cutlist::load(&dir) {
            Ok(c) => c,
            Err(e) => {
                // Don't silently overwrite a cutlist we couldn't parse.
                self.error.set(Some(format!("Could not read the cutlist in {}: {e}", dir.display())));
                return;
            }
        };
        self.cutlist.set(cutlist);
        self.history.write().clear();
        prefs::remember_folder(&dir);
        let pending: Vec<PathBuf> = {
            let scans = self.scans.peek();
            recordings
                .iter()
                .map(|r| r.path.clone())
                .filter(|p| !matches!(scans.get(p), Some(ScanState::Ready(_))))
                .collect()
        };
        self.scanner.peek().set_queue(pending);
        let first = file.and_then(|f| recordings.iter().position(|r| r.path == f)).unwrap_or(0);
        let empty = recordings.is_empty();
        self.recordings.set(recordings);
        self.folder.set(Some(dir));
        self.selected.set(None);
        self.player().unload();
        if !empty {
            self.select(first);
        }
    }

    pub fn select(mut self, i: usize) {
        if *self.selected.peek() == Some(i) || i >= self.recordings.peek().len() {
            return;
        }
        self.selected.set(Some(i));
        self.ab_stop();
        self.cur_split.set(None);
        self.drag.set(None);
        self.editing_title.set(None);
        self.paste.set(None);
        self.player().unload();
        self.pos.set(0);
        self.playing.set(false);
        let span = self.view.peek().span_secs;
        self.view.set(View { start: 0, span_secs: span });

        let path = self.recordings.peek()[i].path.clone();
        {
            let mut cl = self.cutlist.write();
            let edit = cl.recordings.entry(key_of(&path)).or_default();
            if edit.status == Status::Todo {
                edit.status = Status::InProgress;
            }
        }
        self.mark_dirty();
        self.save_now();
        let state = self.scans.peek().get(&path).cloned();
        match state {
            Some(ScanState::Ready(scan)) => {
                self.load(&path, &scan);
                if !self.loudness.peek().contains_key(&path) {
                    self.scanner.peek().prioritize_loudness(path, scan);
                }
            }
            Some(ScanState::Failed(_)) => {}
            _ => self.scanner.peek().prioritize(path),
        }
    }

    /// F: flag the current recording to come back to (or unflag it).
    pub fn toggle_flag(mut self) {
        let Some(path) = self.selected_path() else { return };
        if let Some(edit) = self.cutlist.write().recordings.get_mut(&key_of(&path)) {
            edit.status = if edit.status == Status::Flagged { Status::InProgress } else { Status::Flagged };
        }
        self.mark_dirty();
        self.save_now();
    }

    /// Clear an error message once it has been up for a while.
    fn fade_error(mut self) {
        let shown = self.error.peek().is_some();
        let since = *self.error_since.peek();
        match (shown, since) {
            (true, None) => self.error_since.set(Some(Instant::now())),
            (true, Some(t)) if t.elapsed() > Duration::from_secs(ERROR_SECS) => {
                self.error.set(None);
                self.error_since.set(None);
            }
            (false, Some(_)) => self.error_since.set(None),
            _ => {}
        }
    }

    pub fn select_offset(self, delta: isize) {
        let len = self.recordings.peek().len();
        if len == 0 {
            return;
        }
        let cur = (*self.selected.peek()).map(|i| i as isize).unwrap_or(-1);
        self.select((cur + delta).clamp(0, len as isize - 1) as usize);
    }

    fn load(&self, path: &Path, scan: &Scan) {
        self.player().load(path.to_owned(), scan.mp3.clone().map(Arc::new), scan.info.total_samples);
    }

    pub fn toggle(&self) {
        self.player().toggle();
    }

    pub fn seek(mut self, frame: u64, center: bool) {
        let Some(scan) = self.current_scan() else { return };
        let frame = frame.min(scan.info.total_samples);
        self.player().seek(frame);
        self.pos.set(frame);
        self.reveal(frame, center);
    }

    pub fn nudge(self, secs: f64) {
        let Some(scan) = self.current_scan() else { return };
        let delta = (secs.abs() * scan.info.sample_rate as f64) as u64;
        let pos = *self.pos.peek();
        let target = if secs < 0.0 { pos.saturating_sub(delta) } else { pos + delta };
        self.seek(target, false);
    }

    /// Scroll the detail view so `frame` is visible (centred if `center`).
    pub fn reveal(mut self, frame: u64, center: bool) {
        let Some(scan) = self.current_scan() else { return };
        let v = *self.view.peek();
        let span = (v.span_secs * scan.info.sample_rate as f64) as u64;
        let inside = frame >= v.start && frame < v.start + span;
        if inside && !center {
            return;
        }
        let start = if center { frame.saturating_sub(span / 2) } else { frame.saturating_sub(span / 10) };
        let max_start = scan.info.total_samples.saturating_sub(span);
        self.view.set(View { start: start.min(max_start), ..v });
    }

    pub fn zoom(mut self, factor: f64) {
        let Some(scan) = self.current_scan() else { return };
        let v = *self.view.peek();
        let rate = scan.info.sample_rate as f64;
        let max = View::MAX_SPAN.min(scan.info.duration_secs()).max(View::MIN_SPAN);
        let span_secs = (v.span_secs * factor).clamp(View::MIN_SPAN, max);
        // Keep the playhead at the same relative spot if it's on screen, else zoom around the middle.
        let pos = *self.pos.peek() as f64;
        let old_span = v.span_secs * rate;
        let anchor = if pos >= v.start as f64 && pos < v.start as f64 + old_span {
            pos
        } else {
            v.start as f64 + old_span / 2.0
        };
        let rel = (anchor - v.start as f64) / old_span;
        let start = (anchor - rel * span_secs * rate).max(0.0) as u64;
        let max_start = scan.info.total_samples.saturating_sub((span_secs * rate) as u64);
        self.view.set(View { start: start.min(max_start), span_secs });
    }

    pub fn scroll_view(mut self, frames: i64) {
        let Some(scan) = self.current_scan() else { return };
        let v = *self.view.peek();
        let span = (v.span_secs * scan.info.sample_rate as f64) as u64;
        let max_start = scan.info.total_samples.saturating_sub(span);
        let start = (v.start as i64 + frames).clamp(0, max_start as i64) as u64;
        self.view.set(View { start, ..v });
    }

    pub fn focus_root(&self) {
        if let Some(root) = self.root.peek().clone() {
            spawn(async move {
                let _ = root.set_focus(true).await;
            });
        }
    }

    /// Called every frame: drain scanner events, publish the playhead, follow it.
    pub fn tick(mut self) {
        let scanner = self.scanner.peek().clone();
        while let Some(ev) = scanner.try_recv() {
            match ev {
                ScanEvent::Started(p) => {
                    self.scans.write().insert(p, ScanState::Scanning(0.0));
                }
                ScanEvent::Progress(p, f) => {
                    self.scans.write().insert(p, ScanState::Scanning(f));
                }
                ScanEvent::Done(p, scan) => {
                    self.ensure_detected(&p, &scan);
                    if self.selected_path().as_ref() == Some(&p) {
                        self.load(&p, &scan);
                    }
                    self.scans.write().insert(p, ScanState::Ready(scan));
                }
                ScanEvent::Failed(p, e) => {
                    self.scans.write().insert(p, ScanState::Failed(e));
                }
                ScanEvent::Loudness(p, map) => {
                    self.loudness.write().insert(p, map);
                }
            }
        }

        let player = self.player();
        let pos = player.position();
        if pos != *self.pos.peek() && player.is_loaded() {
            self.pos.set(pos);
        }
        let playing = player.is_playing();
        if playing != *self.playing.peek() {
            self.playing.set(playing);
        }
        if let Some(e) = player.take_error() {
            self.error.set(Some(e));
        }
        if playing {
            self.follow(pos);
        }
        self.poll_export();
        self.fade_error();
        self.poll_ab();
        if self.cut_dirty.peek().is_some_and(|t| t.elapsed() > Duration::from_millis(400)) {
            self.save_now();
        }
    }

    /// Write the cutlist if anything changed.
    pub fn save_now(mut self) {
        if self.cut_dirty.peek().is_none() {
            return;
        }
        self.cut_dirty.set(None);
        let Some(dir) = self.folder.peek().clone() else { return };
        if let Err(e) = self.cutlist.peek().save(&dir) {
            self.error.set(Some(format!("Could not save the cutlist: {e}")));
        }
    }

    /// Page the detail view when the playhead runs off its right edge.
    fn follow(mut self, pos: u64) {
        let Some(scan) = self.current_scan() else { return };
        let v = *self.view.peek();
        let span = (v.span_secs * scan.info.sample_rate as f64) as u64;
        if pos < v.start || pos >= v.start + span * 97 / 100 {
            let max_start = scan.info.total_samples.saturating_sub(span);
            self.view.set(View { start: pos.saturating_sub(span * 3 / 100).min(max_start), ..v });
        }
    }
}

/// Cutlist key for a recording: its file name, so the folder can be moved or renamed.
pub fn key_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Small settings kept between runs (the last opened folder).
pub mod prefs {
    use std::path::{Path, PathBuf};

    fn file() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("splitter").join("last-folder"))
    }

    pub fn remember_folder(dir: &Path) {
        if let Some(f) = file() {
            let _ = std::fs::create_dir_all(f.parent().unwrap());
            let _ = std::fs::write(f, dir.to_string_lossy().as_bytes());
        }
    }

    /// The folder open when the app last closed, if it still exists.
    pub fn last_folder() -> Option<PathBuf> {
        let dir = PathBuf::from(std::fs::read_to_string(file()?).ok()?.trim());
        dir.is_dir().then_some(dir)
    }
}
