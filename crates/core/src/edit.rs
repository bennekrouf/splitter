//! Per-recording edit state: split points, their review state, and undo history.
//!
//! Positions are sample frames at the recording's own sample rate. Track `k` runs from split
//! `k-1` (or the start) to split `k` (or the end), so tracks never overlap or leave gaps.
//! Silence tagged by detection is cut from a track's edges on export unless the user keeps it.

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

/// A quiet stretch found by detection, `[start, end)`. Where it touches a track's edge (the
/// recording's lead-in or tail, or the gap around a split) that part is cut from the export,
/// unless `keep` is set. Silence in the middle of a track is never cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Silence {
    pub start: u64,
    pub end: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep: bool,
}

/// One track as shown and exported: `[start, end)` plus its metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Track<'a> {
    /// 0-based position in the recording (dropped tracks included).
    pub index: usize,
    pub start: u64,
    pub end: u64,
    /// What gets exported: `[start, end)` minus the silence cut at either edge. Empty when the
    /// whole track is cut silence.
    pub audio_start: u64,
    pub audio_end: u64,
    pub meta: &'a TrackMeta,
}

impl Track<'_> {
    pub fn audio_len(&self) -> u64 {
        self.audio_end.saturating_sub(self.audio_start)
    }

    /// Not left out, and not all cut silence.
    pub fn exported(&self) -> bool {
        !self.meta.drop && self.audio_len() > 0
    }
}

/// The undoable part of an edit.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub head: TrackMeta,
    pub splits: Vec<Split>,
    pub silences: Vec<Silence>,
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
    /// Sorted, non-overlapping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub silences: Vec<Silence>,
    /// Whether `silences` has been filled in (cutlists from before silence tagging lack it).
    #[serde(default)]
    pub silences_detected: bool,
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

    /// Replace the tagged silences. One the user chose to keep stays kept if it overlaps a new one.
    pub fn set_silences(&mut self, mut silences: Vec<Silence>) {
        for s in &mut silences {
            s.keep = self.silences.iter().any(|o| o.keep && o.start < s.end && s.start < o.end);
        }
        self.silences = silences;
        self.silences_detected = true;
    }

    /// The silence containing `pos`.
    pub fn silence_at(&self, pos: u64) -> Option<usize> {
        let i = self.silences.partition_point(|s| s.end <= pos);
        self.silences.get(i).is_some_and(|s| s.start <= pos).then_some(i)
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
        starts
            .zip(ends)
            .enumerate()
            .map(|(index, ((start, meta), end))| {
                let cut =
                    |f: fn(&Silence, u64) -> bool, at: u64| self.silences.iter().find(|s| !s.keep && f(s, at)).copied();
                // A silence cuts the start of a track if the track starts inside it, and the end
                // if the track ends inside it (a split in a gap does both, for its two tracks).
                let audio_start = cut(|s, at| s.start <= at && at < s.end, start).map_or(start, |s| s.end.min(end));
                let audio_end = cut(|s, at| s.start < at && at <= s.end, end).map_or(end, |s| s.start.max(start));
                Track { index, start, end, audio_start, audio_end: audio_end.max(audio_start), meta }
            })
            .collect()
    }

    /// What the export leaves out, as merged `[start, end)` ranges: cut silence and dropped
    /// tracks. Playing everything else sounds like the exported tracks back to back.
    pub fn skipped(&self, total: u64) -> Vec<(u64, u64)> {
        let mut out: Vec<(u64, u64)> = Vec::new();
        for t in self.tracks(total) {
            let parts = if t.exported() {
                [(t.start, t.audio_start), (t.audio_end, t.end)]
            } else {
                [(t.start, t.end), (t.end, t.end)]
            };
            for (a, b) in parts.into_iter().filter(|(a, b)| a < b) {
                match out.last_mut() {
                    Some(last) if last.1 >= a => last.1 = last.1.max(b),
                    _ => out.push((a, b)),
                }
            }
        }
        out
    }

    /// The cleaned recording: the exported tracks' audio back to back, as `[start, end)` source
    /// ranges, plus the edit that goes with it (a confirmed split between consecutive tracks,
    /// titles kept, nothing left to cut). Empty if nothing would be kept.
    pub fn cleaned(&self, total: u64) -> (Vec<(u64, u64)>, RecordingEdit) {
        let kept: Vec<Track<'_>> = self.tracks(total).into_iter().filter(|t| t.exported()).collect();
        let ranges = kept.iter().map(|t| (t.audio_start, t.audio_end)).collect();
        let mut edit =
            RecordingEdit { detect: self.detect, detected: true, silences_detected: true, ..Default::default() };
        let mut at = 0;
        for (i, t) in kept.iter().enumerate() {
            let meta = TrackMeta { title: t.meta.title.clone(), drop: false };
            if i == 0 {
                edit.head = meta;
            } else {
                edit.splits.push(Split { track: meta, ..Split::confirmed(at) });
            }
            at += t.audio_len();
        }
        (ranges, edit)
    }

    /// Each track's name as shown and exported: its title, or "Track N" by its number among the
    /// exported tracks. `None` for an untitled track that isn't exported.
    pub fn names(&self, total: u64) -> Vec<Option<String>> {
        let mut n = 0;
        self.tracks(total)
            .iter()
            .map(|t| {
                n += t.exported() as usize;
                match t.meta.title.trim() {
                    "" if t.exported() => Some(format!("Track {n}")),
                    "" => None,
                    title => Some(title.to_string()),
                }
            })
            .collect()
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
        Snapshot { head: self.head.clone(), splits: self.splits.clone(), silences: self.silences.clone() }
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

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

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
        edit.silences = prev.silences;
        self.future.push(old);
        Some(at)
    }

    pub fn redo(&mut self, edit: &mut RecordingEdit) -> Option<Option<usize>> {
        let next = self.future.pop()?;
        let old = edit.snapshot();
        let at = first_difference(&old.splits, &next.splits);
        edit.head = next.head;
        edit.splits = next.splits;
        edit.silences = next.silences;
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
    fn silence_is_cut_from_track_edges() {
        let mut e = RecordingEdit::default();
        e.insert(sugg(50));
        e.set_silences(vec![
            Silence { start: 0, end: 5, keep: false },    // lead-in
            Silence { start: 20, end: 25, keep: false },  // inside track 0: stays
            Silence { start: 45, end: 55, keep: false },  // around the split
            Silence { start: 90, end: 100, keep: false }, // tail
        ]);
        let audio: Vec<(u64, u64)> = e.tracks(100).iter().map(|t| (t.audio_start, t.audio_end)).collect();
        assert_eq!(audio, [(5, 45), (55, 90)]);
        let bounds: Vec<(u64, u64)> = e.tracks(100).iter().map(|t| (t.start, t.end)).collect();
        assert_eq!(bounds, [(0, 50), (50, 100)], "splits are unchanged");

        // Keeping the gap's silence puts it back in both tracks.
        let i = e.silence_at(50).unwrap();
        e.silences[i].keep = true;
        let audio: Vec<(u64, u64)> = e.tracks(100).iter().map(|t| (t.audio_start, t.audio_end)).collect();
        assert_eq!(audio, [(5, 50), (50, 90)]);

        // A track that is all silence ends up empty; re-detecting keeps the user's choice.
        e.insert(sugg(95));
        assert_eq!(e.tracks(100)[2].audio_len(), 0);
        e.set_silences(vec![Silence { start: 46, end: 54, keep: false }]);
        assert!(e.silences[0].keep);
        assert_eq!(e.silence_at(45), None);
        assert_eq!(e.silence_at(54), None);
    }

    #[test]
    fn skipped_is_what_the_export_leaves_out() {
        let mut e = RecordingEdit::default();
        for at in [50, 100] {
            e.insert(sugg(at));
        }
        e.set_silences(vec![
            Silence { start: 0, end: 5, keep: false },
            Silence { start: 45, end: 55, keep: false },
            Silence { start: 95, end: 105, keep: false },
        ]);
        assert_eq!(e.skipped(150), [(0, 5), (45, 55), (95, 105)]);
        // Leaving out the middle track merges it with the silence on both sides.
        e.track_meta_mut(1).unwrap().drop = true;
        assert_eq!(e.skipped(150), [(0, 5), (45, 105)]);
    }

    #[test]
    fn cleaned_keeps_exported_audio_and_titles() {
        let mut e = RecordingEdit::default();
        for at in [50, 100] {
            e.insert(sugg(at));
        }
        e.set_silences(vec![
            Silence { start: 0, end: 5, keep: false },
            Silence { start: 45, end: 55, keep: false },
            Silence { start: 140, end: u64::MAX, keep: false },
        ]);
        e.head.title = "Intro".into();
        e.track_meta_mut(1).unwrap().drop = true; // talk
        e.track_meta_mut(2).unwrap().title = "Song".into();
        let (ranges, c) = e.cleaned(150);
        assert_eq!(ranges, [(5, 45), (100, 140)]);
        let tracks: Vec<(u64, u64, &str)> =
            c.tracks(80).iter().map(|t| (t.start, t.end, t.meta.title.as_str())).collect();
        assert_eq!(tracks, [(0, 40, "Intro"), (40, 80, "Song")]);
        assert_eq!(c.counts(), (1, 0), "already reviewed");
        assert!(c.skipped(80).is_empty(), "nothing left to cut");
    }

    #[test]
    fn untitled_tracks_are_named_by_export_number() {
        let mut e = RecordingEdit::default();
        for at in [10, 20, 30] {
            e.insert(sugg(at));
        }
        e.track_meta_mut(1).unwrap().drop = true;
        e.track_meta_mut(2).unwrap().title = " Song ".into();
        let names = e.names(40);
        assert_eq!(names, [Some("Track 1".into()), None, Some("Song".into()), Some("Track 3".into())]);
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
