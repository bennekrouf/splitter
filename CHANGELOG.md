# Changelog

What changed in each release of **Splitter**, the keyboard-driven tool for
splitting long recordings into tracks.

The public version of this page — with the download for each release — lives at
<https://mayorana.ch/en/apps/splitter/releases>. It is generated from this file
by `scripts/changelog_to_json.py`, so this file is the only place a release note
is written.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each heading is dated on the day its tag was pushed. Releases that carried only
build or packaging work say so rather than being hidden: the version numbers a
user sees in the update prompt should all be accounted for.

## [0.1.6] - 2026-09-24

### Added

- A download log in the sidebar: what yt-dlp printed for the last URL download,
  kept after it ends. It opens by itself when a download fails, so the reason
  can be read instead of guessed.

## [0.1.5] - 2026-09-24

### Added

- Videos: MP4, M4V and MOV files with AAC audio are listed next to the
  recordings and split on their sound, with the picture shown where a recording
  shows its overview waveform.
- Video export: one MP4 clip per kept track, cut exactly on the splits, with
  title, album and track number. The clips are re-encoded by ffmpeg, which is
  fetched on first use (about 30 MB, SHA-256 checked) and never bundled.
- **URL…** can download the video instead of only its audio: picture and sound
  are fetched separately and merged into one MP4.

## [0.1.4] - 2026-09-24

### Added

- An update banner: the app checks mayorana.ch for a newer version and links
  straight to the download for the platform it is running on.

### Changed

- First release with a macOS build, signed with an Apple Developer ID and
  notarized.

## [0.1.3] - 2026-09-24

### Added

- Silence at track edges is cut from the export by default. Detection now tags
  every quiet stretch, lead-in and tail included; where one touches a track's
  edge it is trimmed, keeping 0.25 s on each side so quiet attacks and fades
  survive. **G** keeps a silence or cuts it again. Splits don't move, and the
  track list shows each track's length after the cut.

### Fixed

- Pressing Enter in the **URL…** dialog no longer crashes the app.

## [0.1.2] - 2026-09-24

### Added

- First downloadable release. Silence detection suggests the splits, you review
  each one by ear from the keyboard, then export: MP3 without re-encoding
  (gapless), or MP3/FLAC/WAV with EBU R128 loudness, ReplayGain tags and
  optional normalisation. Titles, a batch export queue and A/B listening
  included.

### Changed

- **URL…** downloads through yt-dlp and Deno instead of ffmpeg. Both are fetched
  on first use and checked against their published SHA-256 sums, and yt-dlp
  updates itself at most once a day. The AAC or MP3 stream is fetched directly
  and AAC is decoded to WAV, so nothing needs installing.

## [0.1.1] - 2026-09-24

### Changed

- Tagged, but the release build failed and nothing was published. The first
  downloadable version is 0.1.2.
