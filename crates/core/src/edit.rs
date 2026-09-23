//! Per-recording edit state: split points, their review state, and undo history.
//!
//! Positions are sample frames at the recording's own sample rate. Track `k` runs from split
//! `k-1` (or the start) to split `k` (or the end), so tracks never overlap or leave gaps.

use crate::Status;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitState {
    /// Proposed by silence detection, not yet reviewed.
    Suggested,
    /// Accepted (or placed by hand).
    Confirmed,
}

/// What the user decided about one track.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackMeta {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Left out of the export (talk, tuning, applause…).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drop: bool,
}

impl TrackMeta {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub at: u64,
    pub state: SplitState,
    /// Length of the silence this suggestion came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silence_secs: Option<f32>,
    /// The track that starts at this split. Keeping it on the split means titles follow
    /// their track when splits are added, moved or removed.
    #[serde(default, skip_serializing_if = "TrackMeta::is_default")]
    pub track: TrackMeta,
}

impl Split {
    pub fn confirmed(at: u64) -> Self {
        Self { at, state: SplitState::Confirmed, silence_secs: None, track: TrackMeta::default() }
    }

    pub fn suggested(at: u64, silence_secs: f32) -> Self {
        Self { at, state: SplitState::Suggested, silence_secs: Some(silence_secs), track: TrackMeta::default() }
    }
}

/// One track as shown and exported: `[start, end)` plus its metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Track<'a> {
    /// 0-based position in the recording (dropped tracks included).
    pub index: usize,
    pub start: u64,
    pub end: u64,
    pub meta: &'a TrackMeta,
}

/// The undoable part of an edit.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub head: TrackMeta,
    pub splits: Vec<Split>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DetectParams {
    /// Loudness below this counts as silence (dBFS, RMS over 50 ms).
    pub threshold_db: f32,
    /// Silences shorter than this are ignored.
    pub min_silence_secs: f32,
    /// Two suggestions closer than this keep only the one with the longer silence.
    pub min_track_secs: f32,
}

impl Default for DetectParams {
    fn default() -> Self {
        Self { threshold_db: -45.0, min_silence_secs: 1.5, min_track_secs: 30.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordingEdit {
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub detect: DetectParams,
    /// Whether detection has run once; later runs only happen on request.
    #[serde(default)]
    pub detected: bool,
    /// The first track (the others hang off their starting split).
    #[serde(default, skip_serializing_if = "TrackMeta::is_default")]
    pub head: TrackMeta,
    /// Sorted by `at`, no duplicates, all strictly inside the recording.
    #[serde(default)]
    pub splits: Vec<Split>,
}

impl RecordingEdit {
    /// Insert keeping order; a split already at exactly `at` is replaced. Returns its index.
    pub fn insert(&mut self, split: Split) -> usize {
        match self.splits.binary_search_by_key(&split.at, |s| s.at) {
            Ok(i) => {
                self.splits[i] = split;
                i
            }
            Err(i) => {
                self.splits.insert(i, split);
                i
            }
        }
    }

    pub fn remove(&mut self, i: usize) -> Option<Split> {
        (i < self.splits.len()).then(|| self.splits.remove(i))
    }

    /// Move split `i` to `at`. Returns its new index (it may pass its neighbours).
    pub fn move_to(&mut self, i: usize, at: u64) -> usize {
        let Some(mut s) = self.remove(i) else { return i };
        s.at = at;
        self.insert(s)
    }

    pub fn confirm(&mut self, i: usize) {
        if let Some(s) = self.splits.get_mut(i) {
            s.state = SplitState::Confirmed;
        }
    }

    /// First split strictly after `pos`.
    pub fn next_after(&self, pos: u64) -> Option<usize> {
        let i = self.splits.partition_point(|s| s.at <= pos);
        (i < self.splits.len()).then_some(i)
    }

    /// Last split strictly before `pos`.
    pub fn prev_before(&self, pos: u64) -> Option<usize> {
        self.splits.partition_point(|s| s.at < pos).checked_sub(1)
    }

    /// Next suggestion to review after index `from` (or from the start), wrapping around.
    pub fn next_to_review(&self, from: Option<usize>) -> Option<usize> {
        let n = self.splits.len();
        let begin = from.map(|i| i + 1).unwrap_or(0);
        (0..n).map(|k| (begin + k) % n).find(|&i| self.splits[i].state == SplitState::Suggested)
    }

    /// Split closest to `pos`, if within `max_dist`.
    pub fn nearest(&self, pos: u64, max_dist: u64) -> Option<usize> {
        self.splits
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.at.abs_diff(pos)))
            .filter(|&(_, d)| d <= max_dist)
            .min_by_key(|&(_, d)| d)
            .map(|(i, _)| i)
    }

    /// (confirmed, suggested)
    pub fn counts(&self) -> (usize, usize) {
        let confirmed = self.splits.iter().filter(|s| s.state == SplitState::Confirmed).count();
        (confirmed, self.splits.len() - confirmed)
    }

    /// Replace all unreviewed suggestions with `suggestions`, dropping any that land within
    /// `min_gap` of a confirmed split (the user already decided that area).
    /// A title or drop flag on a replaced suggestion carries over to a new one nearby.
    pub fn apply_detection(&mut self, suggestions: Vec<Split>, min_gap: u64) {
        let old: Vec<Split> = self.splits.iter().filter(|s| s.state == SplitState::Suggested).cloned().collect();
        self.splits.retain(|s| s.state == SplitState::Confirmed);
        let confirmed: Vec<u64> = self.splits.iter().map(|s| s.at).collect();
        for mut s in suggestions {
            if confirmed.iter().all(|&c| c.abs_diff(s.at) >= min_gap) {
                if let Some(o) = old.iter().filter(|o| !o.track.is_default()).find(|o| o.at.abs_diff(s.at) < min_gap) {
                    s.track = o.track.clone();
                }
                self.insert(s);
            }
        }
        self.detected = true;
    }

    // The review loop. Each returns the split to select next; `cur` is the selected split and
    // `pos` the playhead, used when nothing is selected.

    /// Tab / Shift+Tab: the neighbouring split.
    pub fn step_from(&self, cur: Option<usize>, pos: u64, forward: bool) -> Option<usize> {
        match (cur, forward) {
            (Some(i), true) => (i + 1 < self.splits.len()).then_some(i + 1),
            (Some(i), false) => i.checked_sub(1),
            (None, true) => self.next_after(pos),
            (None, false) => self.prev_before(pos),
        }
    }

    /// Enter: confirm the selected split, then go to the next one still to review
    /// (or simply the next split once everything is reviewed).
    pub fn confirm_and_next(&mut self, cur: Option<usize>, pos: u64) -> Option<usize> {
        match cur {
            Some(i) => {
                self.confirm(i);
                self.next_to_review(Some(i)).or_else(|| (i + 1 < self.splits.len()).then_some(i + 1))
            }
            None => self.next_to_review(self.prev_before(pos + 1)).or_else(|| self.next_after(pos)),
        }
    }

    /// Backspace: remove the selected split, then go to the next one still to review.
    pub fn delete_and_next(&mut self, cur: usize) -> Option<usize> {
        self.remove(cur)?;
        // Index `cur` now holds the split that followed the removed one.
        self.next_to_review(cur.checked_sub(1))
    }

    /// All tracks of a recording `total` frames long, in order.
    pub fn tracks(&self, total: u64) -> Vec<Track<'_>> {
        let starts = std::iter::once((0, &self.head)).chain(self.splits.iter().map(|s| (s.at.min(total), &s.track)));
        let ends = self.splits.iter().map(|s| s.at.min(total)).chain(std::iter::once(total));
        starts.zip(ends).enumerate().map(|(index, ((start, meta), end))| Track { index, start, end, meta }).collect()
    }

    pub fn track_meta_mut(&mut self, index: usize) -> Option<&mut TrackMeta> {
        match index {
            0 => Some(&mut self.head),
            k => self.splits.get_mut(k - 1).map(|s| &mut s.track),
        }
    }

    /// The track being worked on: the one starting at the selected split, else the one
    /// under the playhead.
    pub fn current_track(&self, cur: Option<usize>, pos: u64) -> usize {
        match cur {
            Some(i) if i < self.splits.len() => i + 1,
            _ => self.splits.partition_point(|s| s.at <= pos),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot { head: self.head.clone(), splits: self.splits.clone() }
    }
}

/// Undo/redo over snapshots of the split list. Splits are small, so snapshots are cheap.
#[derive(Clone, Debug, Default)]
pub struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
}

impl History {
    const LIMIT: usize = 500;

    /// Record the state *before* a change.
    pub fn record(&mut self, before: Snapshot) {
        if self.past.last() == Some(&before) {
            return;
        }
        self.past.push(before);
        if self.past.len() > Self::LIMIT {
            self.past.remove(0);
        }
        self.future.clear();
    }

    /// Restore the previous state. `None` if there was nothing to undo; otherwise the index
    /// of the first split that changed, if any (so the selection can land on it).
    pub fn undo(&mut self, edit: &mut RecordingEdit) -> Option<Option<usize>> {
        let prev = self.past.pop()?;
        let old = edit.snapshot();
        let at = first_difference(&old.splits, &prev.splits);
        edit.head = prev.head;
        edit.splits = prev.splits;
        self.future.push(old);
        Some(at)
    }

    pub fn redo(&mut self, edit: &mut RecordingEdit) -> Option<Option<usize>> {
        let next = self.future.pop()?;
        let old = edit.snapshot();
        let at = first_difference(&old.splits, &next.splits);
        edit.head = next.head;
        edit.splits = next.splits;
        self.past.push(old);
        Some(at)
    }
}

/// First index where `a` and `b` differ, clamped to a valid index of `b`.
fn first_difference(a: &[Split], b: &[Split]) -> Option<usize> {
    if a == b || b.is_empty() {
        return None;
    }
    let i = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    Some(i.min(b.len() - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sugg(at: u64) -> Split {
        Split::suggested(at, 2.0)
    }

    #[test]
    fn stays_sorted_through_moves() {
        let mut e = RecordingEdit::default();
        for at in [300, 100, 200] {
            e.insert(sugg(at));
        }
        assert_eq!(e.splits.iter().map(|s| s.at).collect::<Vec<_>>(), [100, 200, 300]);
        let i = e.move_to(0, 250);
        assert_eq!(i, 1);
        assert_eq!(e.splits.iter().map(|s| s.at).collect::<Vec<_>>(), [200, 250, 300]);
    }

    #[test]
    fn navigation() {
        let mut e = RecordingEdit::default();
        for at in [100, 200, 300] {
            e.insert(sugg(at));
        }
        assert_eq!(e.next_after(100), Some(1));
        assert_eq!(e.next_after(300), None);
        assert_eq!(e.prev_before(100), None);
        assert_eq!(e.prev_before(250), Some(1));
        e.confirm(1);
        assert_eq!(e.next_to_review(Some(0)), Some(2));
        assert_eq!(e.next_to_review(Some(2)), Some(0)); // wraps
        assert_eq!(e.nearest(190, 20), Some(1));
        assert_eq!(e.nearest(150, 20), None);
        assert_eq!(e.counts(), (1, 2));
    }

    #[test]
    fn detection_keeps_confirmed_decisions() {
        let mut e = RecordingEdit::default();
        e.insert(Split::confirmed(1000));
        e.insert(sugg(5000));
        e.apply_detection(vec![sugg(1010), sugg(3000), sugg(9000)], 100);
        assert_eq!(e.splits.iter().map(|s| s.at).collect::<Vec<_>>(), [1000, 3000, 9000]);
        assert!(e.detected);
    }

    #[test]
    fn tracks_cover_everything() {
        let mut e = RecordingEdit::default();
        e.insert(sugg(10));
        e.insert(sugg(30));
        let bounds: Vec<(u64, u64)> = e.tracks(50).iter().map(|t| (t.start, t.end)).collect();
        assert_eq!(bounds, [(0, 10), (10, 30), (30, 50)]);
        assert_eq!(e.current_track(None, 10), 1);
        assert_eq!(e.current_track(None, 9), 0);
        assert_eq!(e.current_track(Some(1), 0), 2);
    }

    #[test]
    fn titles_follow_their_track() {
        let mut e = RecordingEdit::default();
        e.insert(sugg(10));
        e.insert(sugg(30));
        e.head.title = "Intro".into();
        e.track_meta_mut(1).unwrap().title = "Song A".into();
        e.track_meta_mut(2).unwrap().title = "Song B".into();
        // A new split inside Song A creates an untitled track; Song B keeps its title.
        e.insert(Split::confirmed(20));
        let titles: Vec<&str> = e.tracks(50).iter().map(|t| t.meta.title.as_str()).collect();
        assert_eq!(titles, ["Intro", "Song A", "", "Song B"]);
        // Removing the split that starts Song A merges it into Intro.
        e.remove(0);
        let titles: Vec<&str> = e.tracks(50).iter().map(|t| t.meta.title.as_str()).collect();
        assert_eq!(titles, ["Intro", "", "Song B"]);
    }

    #[test]
    fn redetect_keeps_titles_on_nearby_suggestions() {
        let mut e = RecordingEdit::default();
        e.insert(sugg(1000));
        e.track_meta_mut(1).unwrap().title = "Kept".into();
        e.apply_detection(vec![sugg(1050), sugg(5000)], 100);
        assert_eq!(e.splits[0].track.title, "Kept");
        assert_eq!(e.splits[1].track.title, "");
    }

    #[test]
    fn undo_redo() {
        let mut h = History::default();
        let mut e = RecordingEdit::default();
        e.insert(sugg(1));
        h.record(e.snapshot());
        e.insert(sugg(2));
        assert_eq!(h.undo(&mut e), Some(Some(0)));
        assert_eq!(e.splits.len(), 1);
        assert_eq!(h.redo(&mut e), Some(Some(1)), "selects the split that came back");
        assert_eq!(e.splits.len(), 2);
        assert_eq!(h.redo(&mut e), None);
        // Undoing a title change on the first track touches no split.
        h.record(e.snapshot());
        e.head.title = "x".into();
        assert_eq!(h.undo(&mut e), Some(None));
        assert_eq!(e.head.title, "");
    }

    /// Replays a review session the way the keyboard drives it.
    struct Session {
        edit: RecordingEdit,
        history: History,
        cur: Option<usize>,
        pos: u64,
    }

    impl Session {
        fn new(at: &[u64]) -> Self {
            let mut edit = RecordingEdit::default();
            for &a in at {
                edit.insert(sugg(a));
            }
            Self { edit, history: History::default(), cur: None, pos: 0 }
        }
        fn change(&mut self, f: impl FnOnce(&mut RecordingEdit, Option<usize>, u64) -> Option<usize>) {
            let before = self.edit.snapshot();
            self.cur = f(&mut self.edit, self.cur, self.pos);
            if self.edit.snapshot() != before {
                self.history.record(before);
            }
        }
        fn tab(&mut self) {
            self.cur = self.edit.step_from(self.cur, self.pos, true);
        }
        fn enter(&mut self) {
            self.change(|e, cur, pos| e.confirm_and_next(cur, pos));
        }
        fn backspace(&mut self) {
            self.change(|e, cur, _| e.delete_and_next(cur?));
        }
        fn undo(&mut self) {
            if let Some(Some(i)) = self.history.undo(&mut self.edit) {
                self.cur = Some(i);
            }
        }
        fn selected(&self) -> Option<u64> {
            self.cur.map(|i| self.edit.splits[i].at)
        }
        fn ats(&self) -> Vec<u64> {
            self.edit.splits.iter().map(|s| s.at).collect()
        }
    }

    #[test]
    fn tab_tab_delete_undo_restores_and_selects_the_split() {
        let mut s = Session::new(&[100, 200, 300, 400]);
        s.tab();
        assert_eq!(s.selected(), Some(100));
        s.tab();
        assert_eq!(s.selected(), Some(200));
        s.backspace();
        assert_eq!(s.ats(), [100, 300, 400]);
        assert_eq!(s.selected(), Some(300), "moves on to the next one to review");
        s.undo();
        assert_eq!(s.ats(), [100, 200, 300, 400]);
        assert_eq!(s.selected(), Some(200), "selection returns to the restored split");
    }

    #[test]
    fn enter_walks_through_suggestions_then_wraps_to_skipped_ones() {
        let mut s = Session::new(&[100, 200, 300]);
        s.enter(); // nothing selected: go to the first suggestion
        assert_eq!(s.selected(), Some(100));
        s.tab(); // skip 200 without deciding
        s.tab();
        assert_eq!(s.selected(), Some(300));
        s.enter(); // keep 300 -> wraps back to 100, still unreviewed
        assert_eq!(s.selected(), Some(100));
        s.enter();
        assert_eq!(s.selected(), Some(200));
        s.enter();
        assert_eq!(s.edit.counts(), (3, 0));
        assert_eq!(s.selected(), Some(300), "all reviewed: just moves forward");
        s.undo();
        assert_eq!(s.edit.counts(), (2, 1));
        assert_eq!(s.selected(), Some(200), "undo lands on the split whose state changed");
    }

    #[test]
    fn deleting_the_last_suggestion_leaves_nothing_selected() {
        let mut s = Session::new(&[100]);
        s.tab();
        s.backspace();
        assert!(s.edit.splits.is_empty());
        assert_eq!(s.cur, None);
        s.undo();
        assert_eq!(s.selected(), Some(100));
    }

    #[test]
    fn enter_from_the_playhead_starts_at_the_next_suggestion() {
        let mut s = Session::new(&[100, 200, 300]);
        s.pos = 150;
        s.enter();
        assert_eq!(s.selected(), Some(200));
    }
}
