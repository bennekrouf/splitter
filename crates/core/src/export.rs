//! What an export produces: numbering, titles and file names for the kept tracks.

use crate::edit::RecordingEdit;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportSettings {
    /// File name template: `{n}` track number, `{nn}` zero-padded, `{title}`, `{source}`
    /// (the recording's file name without extension).
    pub naming: String,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self { naming: "{nn} - {title}".into() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlannedTrack {
    /// 1-based among exported tracks.
    pub number: usize,
    pub total: usize,
    pub start: u64,
    pub end: u64,
    /// The user's title, or "Track N".
    pub title: String,
    /// File name without extension.
    pub stem: String,
}

pub fn plan(edit: &RecordingEdit, total_samples: u64, source_stem: &str, settings: &ExportSettings) -> Vec<PlannedTrack> {
    let kept: Vec<_> = edit.tracks(total_samples).into_iter().filter(|t| !t.meta.drop && t.end > t.start).collect();
    let total = kept.len();
    let width = total.to_string().len().max(2);
    let mut out: Vec<PlannedTrack> = Vec::with_capacity(total);
    for (i, t) in kept.into_iter().enumerate() {
        let number = i + 1;
        let title = if t.meta.title.trim().is_empty() { format!("Track {number}") } else { t.meta.title.trim().to_string() };
        let name = settings
            .naming
            .replace("{nn}", &format!("{number:0width$}"))
            .replace("{n}", &number.to_string())
            .replace("{title}", &title)
            .replace("{source}", source_stem);
        let mut stem = sanitize(&name);
        // Two tracks with the same title would overwrite each other.
        if out.iter().any(|p| p.stem.eq_ignore_ascii_case(&stem)) {
            stem = format!("{stem} ({number})");
        }
        out.push(PlannedTrack { number, total, start: t.start, end: t.end, title, stem });
    }
    out
}

/// Make a string safe as a file name on macOS, Windows and Linux.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    let short: String = trimmed.chars().take(180).collect();
    if short.is_empty() { "untitled".into() } else { short }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Split;

    #[test]
    fn numbers_kept_tracks_and_names_files() {
        let mut e = RecordingEdit::default();
        for at in [100, 200, 300] {
            e.insert(Split::confirmed(at));
        }
        e.head.title = "Intro".into();
        e.track_meta_mut(1).unwrap().drop = true;
        e.track_meta_mut(2).unwrap().title = "AC/DC: Live?".into();
        let p = plan(&e, 400, "set", &ExportSettings::default());
        let names: Vec<&str> = p.iter().map(|t| t.stem.as_str()).collect();
        assert_eq!(names, ["01 - Intro", "02 - AC_DC_ Live_", "03 - Track 3"]);
        assert_eq!((p[1].start, p[1].end, p[1].total), (200, 300, 3));
    }

    #[test]
    fn duplicate_titles_get_distinct_names() {
        let mut e = RecordingEdit::default();
        e.insert(Split::confirmed(100));
        e.head.title = "Jam".into();
        e.track_meta_mut(1).unwrap().title = "Jam".into();
        let s = ExportSettings { naming: "{title}".into() };
        let names: Vec<String> = plan(&e, 200, "set", &s).into_iter().map(|t| t.stem).collect();
        assert_eq!(names, ["Jam", "Jam (2)"]);
    }

    #[test]
    fn sanitize_never_returns_empty() {
        assert_eq!(sanitize(" ... "), "untitled");
        assert_eq!(sanitize("a\u{7}b"), "a_b");
    }
}
