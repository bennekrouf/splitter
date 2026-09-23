//! Turning a pasted tracklist into titles.

use crate::edit::RecordingEdit;

/// One title per non-empty line, with leading numbering / timestamps and trailing durations
/// removed. Handles the usual shapes:
///
/// ```text
/// 1. Title            01 - Title          03:12 Title
/// [00:03:12] Title    Title (4:32)        Title [4:32]
/// ```
pub fn parse(text: &str) -> Vec<String> {
    text.lines().map(clean_line).filter(|t| !t.is_empty()).collect()
}

fn clean_line(line: &str) -> String {
    let mut s = line.trim();
    // Leading tokens: track numbers ("1.", "01", "1)", "#3") and timestamps ("3:12", "[01:02:03]").
    loop {
        let before = s;
        s = s.trim_start_matches(['-', '–', '—', '.', ')', ':', '|', '•', '*', '#']).trim_start();
        if let Some(rest) = strip_leading_token(s) {
            s = rest;
        }
        if s == before {
            break;
        }
    }
    // Trailing duration: "(4:32)", "[4:32]", "4:32".
    let mut s = s.trim_end();
    for (open, close) in [('(', ')'), ('[', ']')] {
        if let Some(inner) = s.strip_suffix(close).and_then(|x| x.rfind(open).map(|i| (&x[..i], &x[i + 1..]))) {
            if is_time(inner.1.trim()) {
                s = inner.0.trim_end();
            }
        }
    }
    if let Some((head, last)) = s.rsplit_once(char::is_whitespace) {
        if is_time(last) {
            s = head.trim_end();
        }
    }
    s.trim_end_matches(['-', '–', '—', '|']).trim().to_string()
}

/// Strip one leading number / timestamp token (optionally bracketed) if it is followed by
/// a separator or space.
fn strip_leading_token(s: &str) -> Option<&str> {
    let (token, rest) = if let Some(r) = s.strip_prefix('[').or_else(|| s.strip_prefix('(')) {
        let end = r.find([']', ')'])?;
        (&r[..end], &r[end + 1..])
    } else {
        let end = s.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(s.len());
        (&s[..end], &s[end..])
    };
    let token = token.trim();
    let is_number = !token.is_empty() && token.chars().all(|c| c.is_ascii_digit()) && token.len() <= 3;
    if !(is_number || is_time(token)) {
        return None;
    }
    // "1984 Song" should keep its year: require a separator or space after a bare number.
    let next = rest.chars().next();
    match next {
        None => Some(rest),
        Some(c) if c.is_whitespace() || ".-–—):|".contains(c) => Some(rest),
        _ => None,
    }
}

fn is_time(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts.iter().all(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
}

/// Give the kept (non-dropped) tracks the pasted titles, in order. Returns how many were set.
pub fn apply(edit: &mut RecordingEdit, titles: &[String]) -> usize {
    let kept: Vec<usize> = (0..=edit.splits.len())
        .filter(|&k| edit.track_meta_mut(k).is_some_and(|m| !m.drop))
        .collect();
    let mut n = 0;
    for (k, title) in kept.into_iter().zip(titles) {
        if let Some(m) = edit.track_meta_mut(k) {
            m.title = title.clone();
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Split;

    #[test]
    fn cleans_common_formats() {
        let text = "1. Intro\n02 - Blue Train\n03:12 Moment's Notice\n[01:02:03] Locomotion (7:14)\n\
                    #5) I'm Old Fashioned [7:58]\n  \n1984 Song\nLazy Bird 7:00\n7) Artist - Title\n";
        assert_eq!(
            parse(text),
            [
                "Intro",
                "Blue Train",
                "Moment's Notice",
                "Locomotion",
                "I'm Old Fashioned",
                "1984 Song",
                "Lazy Bird",
                "Artist - Title",
            ]
        );
    }

    #[test]
    fn titles_go_to_kept_tracks_only() {
        let mut e = RecordingEdit::default();
        e.insert(Split::confirmed(10));
        e.insert(Split::confirmed(20));
        e.track_meta_mut(1).unwrap().drop = true; // talk between songs
        let n = apply(&mut e, &["A".into(), "B".into(), "C".into()]);
        assert_eq!(n, 2);
        let titles: Vec<&str> = e.tracks(30).iter().map(|t| t.meta.title.as_str()).collect();
        assert_eq!(titles, ["A", "", "B"]);
    }
}
