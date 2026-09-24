//! Apply cuts: write a copy of the recording without its cut silence and left-out tracks, as a
//! new entry next to the original (which is never touched). ⌘Z right after removes the copy.
//! A recording's copy is a WAV; a video's is an MP4, re-encoded by ffmpeg.

use crate::state::{key_of, App};
use crate::tools;
use crate::video_export::{write_ranges, Timeline};
use crossbeam_channel::Receiver;
use dioxus::prelude::*;
use std::path::{Path, PathBuf};

enum Event {
    Progress(f32),
    /// Written. The copy's timeline starts this many frames in (a video's AAC priming).
    Done(u64),
    Failed(String),
}

/// A copy being written in the background.
pub struct Applying {
    events: Receiver<Event>,
    pub progress: f32,
    from: PathBuf,
    to: PathBuf,
    edit: splitter_core::edit::RecordingEdit,
}

/// `name (cleaned).wav` (`.mp4` for a video) next to the original, or `(cleaned 2)`, … if taken.
fn cleaned_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = if splitter_core::is_video(path) { "mp4" } else { "wav" };
    (1..)
        .map(|n| match n {
            1 => path.with_file_name(format!("{stem} (cleaned).{ext}")),
            n => path.with_file_name(format!("{stem} (cleaned {n}).{ext}")),
        })
        .find(|p| !p.exists())
        .unwrap()
}

impl App {
    /// Whether the current recording has anything to cut (and isn't being cleaned already).
    pub fn can_apply_cuts(&self) -> bool {
        let Some(scan) = self.current_scan() else { return false };
        self.applying.peek().is_none()
            && self.peek_edit(|e| !e.skipped(scan.info.total_samples).is_empty()).unwrap_or(false)
    }

    pub fn apply_cuts(mut self) {
        if self.applying.peek().is_some() {
            return;
        }
        let (Some(from), Some(scan)) = (self.selected_path(), self.current_scan()) else { return };
        let total = scan.info.total_samples;
        let Some((ranges, edit)) = self.peek_edit(|e| e.cleaned(total)) else { return };
        if ranges.is_empty() {
            self.error.set(Some("Every track is left out: there is nothing to keep.".into()));
            return;
        }
        if self.peek_edit(|e| e.skipped(total).is_empty()).unwrap_or(true) {
            self.error.set(Some("Nothing is cut in this recording yet.".into()));
            return;
        }
        let to = cleaned_path(&from);
        let video = splitter_core::is_video(&from).then(|| self.cutlist.peek().export.video);
        let (tx, events) = crossbeam_channel::unbounded();
        let (src, dst) = (from.clone(), to.clone());
        std::thread::spawn(move || {
            let mut last = 0.0;
            let mut progress = |p: f32| {
                if p - last >= 0.01 {
                    last = p;
                    let _ = tx.send(Event::Progress(p));
                }
            };
            let rate = scan.info.sample_rate;
            let r = match video {
                Some(profile) => tools::ensure_ffmpeg(&tools::dir(), &mut |_| {}, &|| false).and_then(|ffmpeg| {
                    write_ranges(&ffmpeg, &src, Timeline::of(&src, rate), &ranges, &dst, profile, &mut progress)
                        .map(|()| Timeline::of(&dst, rate).frame(0.0))
                }),
                None => splitter_audio::transcode::write_ranges_to_wav(&src, &scan, &ranges, &dst, &mut progress)
                    .map(|()| 0)
                    .map_err(|e| format!("{e:#}")),
            };
            let _ = tx.send(match r {
                Ok(shift) => Event::Done(shift),
                Err(e) => Event::Failed(format!("Could not apply the cuts: {e}")),
            });
        });
        self.applying.set(Some(Applying { events, progress: 0.0, from, to, edit }));
    }

    /// Called every frame while a copy is being written.
    pub fn poll_apply(mut self) {
        loop {
            let event = match self.applying.peek().as_ref() {
                Some(a) => a.events.try_recv(),
                None => return,
            };
            match event {
                Ok(Event::Progress(p)) => {
                    if let Some(a) = self.applying.write().as_mut() {
                        a.progress = p;
                    }
                }
                Ok(Event::Done(shift)) => {
                    let Some(mut a) = self.applying.take() else { return };
                    for s in &mut a.edit.splits {
                        s.at += shift;
                    }
                    self.cutlist.write().recordings.insert(key_of(&a.to), a.edit);
                    self.mark_dirty();
                    self.save_now();
                    self.refresh_recordings(&a.to);
                    self.last_apply.set(Some((a.from, a.to)));
                    return;
                }
                Ok(Event::Failed(e)) => {
                    self.applying.set(None);
                    self.error.set(Some(e));
                    return;
                }
                Err(_) => return,
            }
        }
    }

    /// ⌘Z on a copy just made by Apply cuts, with no edits of its own to undo: remove it and go
    /// back to the original. Returns whether it did.
    pub fn undo_apply(mut self) -> bool {
        let Some((from, to)) = self.last_apply.peek().clone() else { return false };
        if self.selected_path().as_ref() != Some(&to)
            || self.history.peek().get(&key_of(&to)).is_some_and(|h| h.can_undo())
        {
            return false;
        }
        self.player().unload();
        if let Err(e) = std::fs::remove_file(&to) {
            self.error.set(Some(format!("Could not remove {}: {e}", key_of(&to))));
            return true;
        }
        self.cutlist.write().recordings.remove(&key_of(&to));
        self.history.write().remove(&key_of(&to));
        self.scans.write().remove(&to);
        self.loudness.write().remove(&to);
        self.last_apply.set(None);
        self.mark_dirty();
        self.save_now();
        self.refresh_recordings(&from);
        true
    }
}
