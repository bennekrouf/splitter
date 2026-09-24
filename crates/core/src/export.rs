//! What an export produces: numbering, titles and file names for the kept tracks.

use crate::edit::RecordingEdit;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportSettings {
    /// File name template: `{n}` track number, `{nn}` zero-padded, `{title}`, `{source}`
    /// (the recording's file name without extension).
    pub naming: String,
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub normalize: Normalize,
    /// Format of a video's clips (`profile` is for audio recordings).
    #[serde(default)]
    pub video: VideoProfile,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            naming: "{nn} - {title}".into(),
            profile: Profile::Original,
            normalize: Normalize::Off,
            video: VideoProfile::default(),
        }
    }
}

/// Output of a video's export: MP4 clips cut exactly on the splits, so re-encoded (H.264 and
/// AAC; a copy could only cut on keyframes, often seconds away).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VideoProfile {
    /// H.264 at constant quality `crf` (lower is better; 18 looks the same as the source).
    Mp4 { crf: u8 },
}

impl Default for VideoProfile {
    fn default() -> Self {
        VideoProfile::Mp4 { crf: 18 }
    }
}

impl VideoProfile {
    pub const CHOICES: [VideoProfile; 3] =
        [VideoProfile::Mp4 { crf: 18 }, VideoProfile::Mp4 { crf: 23 }, VideoProfile::Mp4 { crf: 28 }];

    pub fn label(&self) -> String {
        match self {
            VideoProfile::Mp4 { crf: ..=18 } => "MP4 · high quality".into(),
            VideoProfile::Mp4 { crf: 19..=23 } => "MP4 · standard".into(),
            VideoProfile::Mp4 { .. } => "MP4 · small files".into(),
        }
    }

    pub fn extension(&self) -> &'static str {
        "mp4"
    }

    /// Audio bitrate of the clips.
    pub fn audio_kbps(&self) -> u32 {
        192
    }
}

/// Loudness adjustment applied when re-encoding (a byte copy can't change level; it gets
/// ReplayGain tags instead).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Normalize {
    #[default]
    Off,
    /// Every track to `target` LUFS: even levels, but quiet songs get as loud as loud ones.
    Track { target: f32 },
    /// One gain for the whole recording so its kept tracks average `target` LUFS; keeps the
    /// dynamics between songs, usually right for a live set.
    Album { target: f32 },
}

impl Normalize {
    /// Never let normalization push the true peak above this.
    pub const CEILING_DB: f64 = -1.0;
    pub const CHOICES: [Normalize; 5] = [
        Normalize::Off,
        Normalize::Album { target: -14.0 },
        Normalize::Album { target: -16.0 },
        Normalize::Track { target: -14.0 },
        Normalize::Track { target: -16.0 },
    ];

    pub fn label(&self) -> String {
        match self {
            Normalize::Off => "Loudness unchanged".into(),
            Normalize::Album { target } => format!("Whole recording to {target} LUFS"),
            Normalize::Track { target } => format!("Each track to {target} LUFS"),
        }
    }
}

/// Output format of an export.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Profile {
    /// Lossless copy in the source's own format (MP3 frames or WAV bytes).
    #[default]
    Original,
    /// LAME VBR, `-V quality` (0 = best).
    Mp3Vbr {
        quality: u8,
    },
    Mp3Cbr {
        kbps: u16,
    },
    Flac,
    /// 16-bit PCM WAV.
    Wav16,
}

/// What the size estimate and warnings need to know about the source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceFacts {
    /// MP3 (or another lossy codec, e.g. AAC in a video).
    pub lossy: bool,
    /// Original can copy the audio as is (MP3, WAV); not audio inside a video file.
    pub copyable: bool,
    /// Average bitrate of the file's audio.
    pub kbps: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Bits per sample for PCM sources.
    pub bits: Option<u32>,
}

impl Profile {
    pub const CHOICES: [Profile; 10] = [
        Profile::Original,
        Profile::Mp3Vbr { quality: 0 },
        Profile::Mp3Vbr { quality: 2 },
        Profile::Mp3Vbr { quality: 4 },
        Profile::Mp3Cbr { kbps: 320 },
        Profile::Mp3Cbr { kbps: 256 },
        Profile::Mp3Cbr { kbps: 192 },
        Profile::Mp3Cbr { kbps: 128 },
        Profile::Flac,
        Profile::Wav16,
    ];

    pub fn label(&self) -> String {
        match self {
            Profile::Original => "Original (lossless copy)".into(),
            Profile::Mp3Vbr { quality } => format!("MP3 VBR V{quality} (~{} kbps)", vbr_kbps(*quality)),
            Profile::Mp3Cbr { kbps } => format!("MP3 CBR {kbps} kbps"),
            Profile::Flac => "FLAC (lossless)".into(),
            Profile::Wav16 => "WAV 16-bit".into(),
        }
    }

    /// Short name for compact places, e.g. the A/B indicator.
    pub fn short(&self) -> String {
        match self {
            Profile::Original => "Original".into(),
            Profile::Mp3Vbr { quality } => format!("MP3 V{quality}"),
            Profile::Mp3Cbr { kbps } => format!("MP3 {kbps}k"),
            Profile::Flac => "FLAC".into(),
            Profile::Wav16 => "WAV 16".into(),
        }
    }

    pub fn extension<'a>(&self, source_ext: &'a str) -> &'a str {
        match self {
            Profile::Original => source_ext,
            Profile::Mp3Vbr { .. } | Profile::Mp3Cbr { .. } => "mp3",
            Profile::Flac => "flac",
            Profile::Wav16 => "wav",
        }
    }

    /// The profile actually used for `src`: Original falls back to FLAC when the audio can't be
    /// copied as is, which keeps the decoded audio exactly.
    pub fn effective(self, src: &SourceFacts) -> Profile {
        match self {
            Profile::Original if !src.copyable => Profile::Flac,
            p => p,
        }
    }

    /// Whether the output can differ audibly from the source.
    pub fn is_lossy(&self) -> bool {
        matches!(self, Profile::Mp3Vbr { .. } | Profile::Mp3Cbr { .. })
    }

    /// Estimated output size for `secs` of audio.
    pub fn estimate_bytes(&self, src: &SourceFacts, secs: f64) -> u64 {
        if *self != self.effective(src) {
            return self.effective(src).estimate_bytes(src, secs);
        }
        let pcm16 = src.sample_rate as f64 * src.channels as f64 * 2.0 * secs;
        let bytes = match self {
            Profile::Original => src.kbps as f64 * 125.0 * secs,
            Profile::Mp3Vbr { quality } => vbr_kbps(*quality) as f64 * 125.0 * secs,
            Profile::Mp3Cbr { kbps } => *kbps as f64 * 125.0 * secs,
            // Typical for music; decoded MP3 compresses a little better than that.
            Profile::Flac => pcm16 * src.bits.map_or(1.0, |b| b as f64 / 16.0).max(1.0) * 0.58,
            Profile::Wav16 => pcm16,
        };
        bytes as u64
    }

    /// Things worth knowing before exporting `src` with this profile.
    pub fn warnings(&self, src: &SourceFacts) -> Vec<String> {
        if *self != self.effective(src) {
            return vec![format!(
                "The audio of a video can't be copied as is: Original exports {} here (an MP3 format gives smaller files).",
                self.effective(src).short()
            )];
        }
        let mut w = Vec::new();
        let target_kbps = match self {
            Profile::Mp3Vbr { quality } => Some(vbr_kbps(*quality)),
            Profile::Mp3Cbr { kbps } => Some(*kbps as u32),
            _ => None,
        };
        match (src.lossy, self) {
            (true, Profile::Flac | Profile::Wav16) => w.push(format!(
                "The source is lossy (~{} kbps): {} keeps exactly that quality in a much larger file.",
                src.kbps,
                self.short()
            )),
            (true, p) if p.is_lossy() => {
                w.push(if src.copyable {
                    "Re-encoding MP3 loses a little quality; Original keeps the source exactly.".into()
                } else {
                    "Re-encoding a lossy source loses a little quality.".into()
                });
                if let Some(t) = target_kbps.filter(|&t| t as f64 > src.kbps as f64 * 1.15) {
                    w.push(format!(
                        "The source is only ~{} kbps: {t} kbps makes files bigger without sounding better.",
                        src.kbps
                    ));
                }
            }
            _ => {}
        }
        if *self == Profile::Wav16 && src.bits.is_some_and(|b| b > 16) {
            w.push(format!("Reduces the {}-bit source to 16-bit.", src.bits.unwrap()));
        }
        if self.is_lossy() && src.sample_rate > 48_000 {
            w.push(format!("MP3 tops out at 48 kHz; the {} Hz source will be resampled.", src.sample_rate));
        }
        w
    }
}

/// Typical average bitrate of LAME `-V` presets.
pub fn vbr_kbps(quality: u8) -> u32 {
    [245, 225, 190, 175, 165, 130, 115, 100, 85, 65][quality.min(9) as usize]
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

pub fn plan(
    edit: &RecordingEdit,
    total_samples: u64,
    source_stem: &str,
    settings: &ExportSettings,
) -> Vec<PlannedTrack> {
    let kept: Vec<_> = edit.tracks(total_samples).into_iter().filter(|t| t.exported()).collect();
    let total = kept.len();
    let width = total.to_string().len().max(2);
    let mut out: Vec<PlannedTrack> = Vec::with_capacity(total);
    for (i, t) in kept.into_iter().enumerate() {
        let number = i + 1;
        let title =
            if t.meta.title.trim().is_empty() { format!("Track {number}") } else { t.meta.title.trim().to_string() };
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
        out.push(PlannedTrack { number, total, start: t.audio_start, end: t.audio_end, title, stem });
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
    if short.is_empty() {
        "untitled".into()
    } else {
        short
    }
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
    fn cuts_silence_and_skips_silent_tracks() {
        use crate::edit::Silence;
        let mut e = RecordingEdit::default();
        for at in [100, 150, 250] {
            e.insert(Split::confirmed(at));
        }
        // A long pause between two songs, with a split at each end of it.
        e.set_silences(vec![Silence { start: 90, end: 160, keep: false }]);
        let p = plan(&e, 400, "set", &ExportSettings::default());
        let bounds: Vec<(u64, u64, usize)> = p.iter().map(|t| (t.start, t.end, t.number)).collect();
        assert_eq!(bounds, [(0, 90, 1), (160, 250, 2), (250, 400, 3)]);
    }

    #[test]
    fn duplicate_titles_get_distinct_names() {
        let mut e = RecordingEdit::default();
        e.insert(Split::confirmed(100));
        e.head.title = "Jam".into();
        e.track_meta_mut(1).unwrap().title = "Jam".into();
        let s = ExportSettings { naming: "{title}".into(), ..Default::default() };
        let names: Vec<String> = plan(&e, 200, "set", &s).into_iter().map(|t| t.stem).collect();
        assert_eq!(names, ["Jam", "Jam (2)"]);
    }

    #[test]
    fn profile_warnings_and_sizes() {
        let mp3 = SourceFacts { lossy: true, copyable: true, kbps: 128, sample_rate: 44100, channels: 2, bits: None };
        let wav =
            SourceFacts { lossy: false, copyable: true, kbps: 1411, sample_rate: 44100, channels: 2, bits: Some(16) };
        assert!(Profile::Original.warnings(&mp3).is_empty());
        assert_eq!(Profile::Mp3Cbr { kbps: 320 }.warnings(&mp3).len(), 2, "re-encode + upsized bitrate");
        assert_eq!(Profile::Mp3Cbr { kbps: 128 }.warnings(&mp3).len(), 1);
        assert_eq!(Profile::Flac.warnings(&mp3).len(), 1);
        assert!(Profile::Mp3Vbr { quality: 2 }.warnings(&wav).is_empty());
        // One minute: 128 kbps = 960 kB; CD audio = 10.6 MB.
        assert_eq!(Profile::Original.estimate_bytes(&mp3, 60.0), 960_000);
        assert_eq!(Profile::Wav16.estimate_bytes(&wav, 60.0), 10_584_000);
        assert_eq!(Profile::Flac.extension("wav"), "flac");
        assert_eq!(Profile::Original.extension("wav"), "wav");
    }

    #[test]
    fn original_falls_back_to_flac_for_video() {
        let video =
            SourceFacts { lossy: true, copyable: false, kbps: 128, sample_rate: 48000, channels: 2, bits: None };
        assert_eq!(Profile::Original.effective(&video), Profile::Flac);
        assert_eq!(Profile::Mp3Vbr { quality: 2 }.effective(&video), Profile::Mp3Vbr { quality: 2 });
        assert_eq!(Profile::Original.estimate_bytes(&video, 60.0), Profile::Flac.estimate_bytes(&video, 60.0));
        assert_eq!(Profile::Original.warnings(&video).len(), 1);
        let reencode = Profile::Mp3Cbr { kbps: 128 }.warnings(&video);
        assert!(!reencode[0].contains("Original"), "{reencode:?}");
    }

    #[test]
    fn video_profile_defaults_for_old_cutlists() {
        // Cutlists written before video export have no `video` field.
        let s: ExportSettings = serde_json::from_str(r#"{"naming":"{nn}","profile":{"kind":"flac"}}"#).unwrap();
        assert_eq!(s.video, VideoProfile::Mp4 { crf: 18 });
        let json = serde_json::to_string(&VideoProfile::Mp4 { crf: 23 }).unwrap();
        assert_eq!(json, r#"{"kind":"mp4","crf":23}"#);
        assert_eq!(VideoProfile::CHOICES.map(|p| p.label()).len(), 3);
    }

    #[test]
    fn profile_serializes_readably() {
        let json = serde_json::to_string(&Profile::Mp3Vbr { quality: 2 }).unwrap();
        assert_eq!(json, r#"{"kind":"mp3_vbr","quality":2}"#);
        assert_eq!(serde_json::from_str::<Profile>(r#"{"kind":"flac"}"#).unwrap(), Profile::Flac);
    }

    #[test]
    fn sanitize_never_returns_empty() {
        assert_eq!(sanitize(" ... "), "untitled");
        assert_eq!(sanitize("a\u{7}b"), "a_b");
    }
}
