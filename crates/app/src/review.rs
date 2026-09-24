//! Split review actions: the Tab / Enter / Backspace loop, nudging, snapping, dragging, undo.

use crate::state::{key_of, App};
use dioxus::prelude::*;
use splitter_audio::Scan;
use splitter_core::detect;
use splitter_core::edit::{DetectParams, RecordingEdit, Silence, Split};
use splitter_core::tracklist;
use splitter_core::Status;
use std::path::Path;
use std::time::Instant;

/// Seconds played on each side of a split when reviewing it.
pub const PREVIEW_SECS: f64 = 3.0;
/// Radius searched by snap-to-silence.
const SNAP_SECS: f64 = 0.5;
/// New suggestions this close to a confirmed split are dropped on re-detect.
const KEEP_CLEAR_SECS: f64 = 2.0;

impl App {
    fn current_key(&self) -> Option<String> {
        self.selected_path().map(|p| key_of(&p))
    }

    /// Read the current recording's edit without subscribing.
    pub fn peek_edit<R>(&self, f: impl FnOnce(&RecordingEdit) -> R) -> Option<R> {
        let key = self.current_key()?;
        let cl = self.cutlist.peek();
        Some(f(cl.recordings.get(&key)?))
    }

    pub fn mark_dirty(mut self) {
        self.cut_dirty.set(Some(Instant::now()));
    }

    /// Change the current recording's edit as one undo step, and save right away.
    fn change<R>(self, f: impl FnOnce(&mut RecordingEdit) -> R) -> Option<R> {
        let key = self.current_key()?;
        let mut cutlist = self.cutlist;
        let mut history = self.history;
        let r = {
            let mut cl = cutlist.write();
            let edit = cl.recordings.entry(key.clone()).or_default();
            let before = edit.snapshot();
            let r = f(edit);
            if edit.snapshot() != before {
                history.write().entry(key).or_default().record(before);
            }
            r
        };
        self.mark_dirty();
        self.save_now();
        Some(r)
    }

    /// Change without an undo step; saved by the debounced autosave (used while dragging).
    fn change_quiet<R>(self, f: impl FnOnce(&mut RecordingEdit) -> R) -> Option<R> {
        let key = self.current_key()?;
        let mut cutlist = self.cutlist;
        let r = f(cutlist.write().recordings.entry(key).or_default());
        self.mark_dirty();
        Some(r)
    }

    fn rate(&self) -> Option<f64> {
        self.current_scan().map(|s| s.info.sample_rate as f64)
    }

    /// Run silence detection once per recording, when its scan first becomes available.
    /// Recordings detected before silence tagging existed only get their silences tagged.
    pub fn ensure_detected(self, path: &Path, scan: &Scan) {
        let key = key_of(path);
        let mut cutlist = self.cutlist;
        {
            let mut cl = cutlist.write();
            let edit = cl.recordings.entry(key).or_default();
            if edit.detected && edit.silences_detected {
                return;
            }
            if !edit.detected {
                let suggestions = suggest(scan, &edit.detect);
                edit.apply_detection(suggestions, (KEEP_CLEAR_SECS * scan.info.sample_rate as f64) as u64);
            }
            edit.set_silences(tag(scan, &edit.detect));
        }
        self.mark_dirty();
        self.save_now();
    }

    /// Adjust the current recording's detection settings (applied by `redetect`).
    pub fn set_detect_params(self, f: impl FnOnce(&mut DetectParams)) {
        self.change_quiet(|e| f(&mut e.detect));
    }

    /// Replace unreviewed suggestions using the current settings. Confirmed splits stay.
    pub fn redetect(mut self) {
        let Some(scan) = self.current_scan() else { return };
        let gap = (KEEP_CLEAR_SECS * scan.info.sample_rate as f64) as u64;
        self.change(|e| {
            let suggestions = suggest(&scan, &e.detect);
            e.apply_detection(suggestions, gap);
            e.set_silences(tag(&scan, &e.detect));
        });
        self.cur_split.set(None);
    }

    /// Select split `i`, bring it into view and optionally play across it.
    pub fn select_split(mut self, i: usize, play: bool) {
        let Some(at) = self.peek_edit(|e| e.splits.get(i).map(|s| s.at)).flatten() else { return };
        self.cur_split.set(Some(i));
        self.closed_track.set(None);
        self.reveal(at, true);
        if play {
            self.play_across(at);
        }
    }

    /// Click on a track in the list: select it and play it from its first to its last sample
    /// (without the silence cut at its edges), then stop.
    pub fn play_track(mut self, k: usize) {
        let Some(scan) = self.current_scan() else { return };
        let total = scan.info.total_samples;
        let Some((a, b)) = self
            .peek_edit(|e| {
                e.tracks(total).get(k).map(|t| match t.audio_len() {
                    0 => (t.start, t.end), // all cut silence: play what's there
                    _ => (t.audio_start, t.audio_end),
                })
            })
            .flatten()
        else {
            return;
        };
        // Track k starts at split k-1; the first track has none.
        match k.checked_sub(1) {
            Some(i) => self.select_split(i, false),
            None => {
                self.cur_split.set(None);
                self.closed_track.set(None);
            }
        }
        self.player().play_range(a, b);
        self.pos.set(a);
        self.reveal(a, false);
    }

    pub fn play_across(mut self, at: u64) {
        let Some(rate) = self.rate() else { return };
        let d = (PREVIEW_SECS * rate) as u64;
        let from = at.saturating_sub(d);
        self.player().play_range(from, at + d);
        self.pos.set(from);
    }

    /// C: listen across the selected split again (or the one nearest the playhead).
    pub fn replay_split(self) {
        let pos = *self.pos.peek();
        let i = (*self.cur_split.peek()).or_else(|| self.peek_edit(|e| e.nearest(pos, u64::MAX)).flatten());
        if let Some(i) = i {
            self.select_split(i, true);
        }
    }

    /// Tab / Shift+Tab: move through splits in order.
    pub fn step_split(self, forward: bool) {
        let pos = *self.pos.peek();
        let cur = *self.cur_split.peek();
        if let Some(i) = self.peek_edit(|e| e.step_from(cur, pos, forward)).flatten() {
            self.select_split(i, true);
        }
    }

    /// Enter: keep the selected split, then go to the next one still to review.
    pub fn confirm_and_next(self) {
        let pos = *self.pos.peek();
        let cur = *self.cur_split.peek();
        if let Some(i) = self.change(|e| e.confirm_and_next(cur, pos)).flatten() {
            self.select_split(i, true);
        }
    }

    /// Backspace: drop the selected split, then go to the next one still to review.
    pub fn delete_split(mut self) {
        let Some(i) = *self.cur_split.peek() else { return };
        let next = self.change(|e| e.delete_and_next(i)).flatten();
        self.cur_split.set(None);
        self.closed_track.set(None);
        if let Some(next) = next {
            self.select_split(next, true);
        }
    }

    /// , / . : move the selected split.
    pub fn nudge_split(mut self, secs: f64) {
        let (Some(i), Some(rate)) = (*self.cur_split.peek(), self.rate()) else { return };
        let Some(scan) = self.current_scan() else { return };
        let delta = (secs * rate) as i64;
        let moved = self.change(|e| {
            let at = (e.splits.get(i)?.at as i64 + delta).clamp(1, scan.info.total_samples as i64 - 1);
            Some(e.move_to(i, at as u64))
        });
        if let Some(Some(j)) = moved {
            self.cur_split.set(Some(j));
        }
    }

    /// S: move the selected split to the quietest point nearby.
    pub fn snap_split(mut self) {
        let Some(i) = *self.cur_split.peek() else { return };
        let Some(scan) = self.current_scan() else { return };
        let l = &scan.loudness;
        let moved = self.change(|e| {
            let at = e.splits.get(i)?.at;
            let to = detect::snap(&l.db, l.window, scan.info.sample_rate, at, SNAP_SECS);
            Some(e.move_to(i, to))
        });
        if let Some(Some(j)) = moved {
            self.cur_split.set(Some(j));
        }
    }

    /// M: add a (confirmed) split at the playhead. The part it closes (from the previous split
    /// to here) becomes the current track, so X right after cuts exactly that part.
    pub fn add_split_at_playhead(mut self) {
        let Some(scan) = self.current_scan() else { return };
        let pos = *self.pos.peek();
        if pos == 0 || pos >= scan.info.total_samples {
            return;
        }
        if let Some(i) = self.change(|e| e.insert(Split::confirmed(pos))) {
            self.cur_split.set(Some(i));
            // Track i runs from split i-1 (or the start) to split i.
            self.closed_track.set(Some(i));
        }
    }

    pub fn undo(self) {
        if !self.undo_apply() {
            self.apply_history(true);
        }
    }

    pub fn redo(self) {
        self.apply_history(false);
    }

    fn apply_history(mut self, undo: bool) {
        let Some(key) = self.current_key() else { return };
        let changed = {
            let mut hist = self.history.write();
            let h = hist.entry(key.clone()).or_default();
            let mut cl = self.cutlist.write();
            let edit = cl.recordings.entry(key).or_default();
            if undo {
                h.undo(edit)
            } else {
                h.redo(edit)
            }
        };
        let Some(changed_split) = changed else { return };
        self.closed_track.set(None);
        self.mark_dirty();
        self.save_now();
        let len = self.peek_edit(|e| e.splits.len()).unwrap_or(0);
        match changed_split {
            Some(i) => self.select_split(i, false),
            None if self.cur_split.peek().is_some_and(|i| i >= len) => self.cur_split.set(len.checked_sub(1)),
            None => {}
        }
    }

    /// ⌘Enter: mark this recording done and move to the next one.
    pub fn finish_recording(self) {
        self.change_quiet(|e| e.status = Status::Done);
        self.save_now();
        self.select_offset(1);
    }

    // Marker dragging

    pub fn split_near(&self, frame: u64, max_dist: u64) -> Option<usize> {
        self.peek_edit(|e| e.nearest(frame, max_dist)).flatten()
    }

    pub fn dragging(&self) -> bool {
        self.drag.peek().is_some()
    }

    pub fn begin_drag(mut self, i: usize) {
        let Some(before) = self.peek_edit(|e| e.snapshot()) else { return };
        self.drag.set(Some(before));
        self.cur_split.set(Some(i));
    }

    pub fn drag_to(mut self, frame: u64) {
        let Some(i) = *self.cur_split.peek() else { return };
        let Some(scan) = self.current_scan() else { return };
        let frame = frame.clamp(1, scan.info.total_samples.saturating_sub(1));
        if let Some(j) = self.change_quiet(|e| e.move_to(i, frame)) {
            self.cur_split.set(Some(j));
        }
    }

    pub fn end_drag(mut self) {
        let Some(before) = self.drag.take() else { return };
        let Some(key) = self.current_key() else { return };
        let changed = self.peek_edit(|e| e.snapshot() != before).unwrap_or(false);
        if changed {
            self.history.write().entry(key).or_default().record(before);
            self.mark_dirty();
            self.save_now();
        }
    }
}

impl App {
    /// The track T and X act on: the one after the selected split, else the one under the playhead.
    pub fn current_track(&self) -> Option<usize> {
        let (cur, pos, closed) = (*self.cur_split.peek(), *self.pos.peek(), *self.closed_track.peek());
        self.peek_edit(|e| track_in_focus(e, cur, closed, pos))
    }

    pub fn set_title(self, track: usize, title: String) {
        self.change(|e| {
            if let Some(m) = e.track_meta_mut(track) {
                m.title = title.trim().to_string();
            }
        });
    }

    /// X: leave a track out of the export (or bring it back).
    pub fn toggle_drop(self, track: Option<usize>) {
        let Some(k) = track.or_else(|| self.current_track()) else { return };
        self.change(|e| {
            if let Some(m) = e.track_meta_mut(k) {
                m.drop = !m.drop;
            }
        });
    }

    /// G: cut the silence at the selected split (or under the playhead) from the export, or keep it.
    pub fn toggle_silence(self) {
        let pos = *self.pos.peek();
        let at = (*self.cur_split.peek()).and_then(|i| self.peek_edit(|e| e.splits.get(i).map(|s| s.at)).flatten());
        self.change(|e| {
            if let Some(i) = e.silence_at(at.unwrap_or(pos)) {
                e.silences[i].keep = !e.silences[i].keep;
            }
        });
    }

    /// T: type the current track's title.
    pub fn edit_title(mut self, track: Option<usize>) {
        if let Some(k) = track.or_else(|| self.current_track()) {
            self.editing_title.set(Some(k));
        }
    }

    /// Titles from a pasted list go to the kept tracks in order. Returns how many were set.
    pub fn apply_tracklist(self, text: &str) -> usize {
        let titles = tracklist::parse(text);
        self.change(|e| tracklist::apply(e, &titles)).unwrap_or(0)
    }
}

/// The track T and X act on: the one M just closed, else the one after the selected split,
/// else the one under the playhead.
pub fn track_in_focus(e: &RecordingEdit, cur: Option<usize>, closed: Option<usize>, pos: u64) -> usize {
    closed.filter(|&k| k <= e.splits.len()).unwrap_or_else(|| e.current_track(cur, pos))
}

fn tag(scan: &Scan, params: &DetectParams) -> Vec<Silence> {
    let l = &scan.loudness;
    detect::tag(&l.db, l.window, scan.info.sample_rate, params)
}

fn suggest(scan: &Scan, params: &DetectParams) -> Vec<Split> {
    let l = &scan.loudness;
    detect::suggest(&l.db, l.window, scan.info.sample_rate, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M at 3:00, M at 4:00, X: exactly 3:00–4:00 is cut, even with the playhead past 4:00.
    #[test]
    fn m_m_x_cuts_between_the_two_marks() {
        let rate = 44_100;
        let mut e = RecordingEdit::default();
        let mut closed = None;
        for secs in [180, 240] {
            let i = e.insert(Split::confirmed(secs * rate));
            closed = Some(i);
        }
        let k = track_in_focus(&e, Some(1), closed, 250 * rate);
        e.track_meta_mut(k).unwrap().drop = true;
        let cut: Vec<(u64, u64)> =
            e.tracks(600 * rate).iter().filter(|t| t.meta.drop).map(|t| (t.start, t.end)).collect();
        assert_eq!(cut, [(180 * rate, 240 * rate)]);
        // With nothing just closed, the selected split still means the track after it.
        assert_eq!(track_in_focus(&e, Some(0), None, 0), 1);
    }
}
