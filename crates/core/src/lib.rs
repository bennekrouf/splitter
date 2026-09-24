//! Project model: the queue of recordings and their review state.
//! No audio and no UI here, so it stays trivially testable.

pub mod cutlist;
pub mod detect;
pub mod edit;
pub mod export;
pub mod loudness;
pub mod time;
pub mod tracklist;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav"];
/// Video files whose audio is split (AAC in MP4/QuickTime); tracks are exported as audio.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Todo,
    InProgress,
    Done,
    Flagged,
    Exported,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    pub path: PathBuf,
}

impl Recording {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| list.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn is_audio(path: &Path) -> bool {
    has_extension(path, AUDIO_EXTENSIONS)
}

pub fn is_video(path: &Path) -> bool {
    has_extension(path, VIDEO_EXTENSIONS)
}

/// A file the app can open: audio, or a video whose audio it splits.
pub fn is_recording(path: &Path) -> bool {
    is_audio(path) || is_video(path)
}

/// `path` with its file name changed to `stem` (made safe for a file name), keeping the
/// extension. `None` if nothing would change.
pub fn renamed(path: &Path, stem: &str) -> Option<PathBuf> {
    let stem = export::sanitize(stem);
    let name = match path.extension() {
        Some(ext) => format!("{stem}.{}", ext.to_string_lossy()),
        None => stem,
    };
    let to = path.with_file_name(name);
    (to != path).then_some(to)
}

/// All supported audio and video files directly inside `dir`, sorted by file name.
pub fn list_recordings(dir: &Path) -> std::io::Result<Vec<Recording>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_recording(p))
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("._")))
        .collect();
    paths.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    Ok(paths.into_iter().map(Recording::new).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renaming_keeps_the_extension_and_folder() {
        let p = Path::new("/music/Live at the Roxy (Official Video).mp3");
        assert_eq!(renamed(p, "Roxy 1976"), Some(PathBuf::from("/music/Roxy 1976.mp3")));
        assert_eq!(renamed(p, " AC/DC: live "), Some(PathBuf::from("/music/AC_DC_ live.mp3")));
        assert_eq!(renamed(p, "Live at the Roxy (Official Video)"), None);
    }

    #[test]
    fn audio_extensions_are_case_insensitive() {
        assert!(is_audio(Path::new("a/Live.MP3")));
        assert!(is_audio(Path::new("b.wav")));
        assert!(!is_audio(Path::new("c.flac")));
        assert!(!is_audio(Path::new("noext")));
    }

    #[test]
    fn videos_are_recordings() {
        assert!(is_recording(Path::new("Concert.MP4")));
        assert!(is_recording(Path::new("clip.mov")));
        assert!(is_video(Path::new("clip.m4v")));
        assert!(!is_audio(Path::new("clip.mp4")));
        assert!(!is_recording(Path::new("clip.mkv")));
    }
}
