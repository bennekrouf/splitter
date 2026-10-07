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

## [0.1.19] - 2026-10-07

### Changed

- Splitter now uses the PolyForm Noncommercial License 1.0.0 and has updated
  branding. This means the software is free for personal use, but commercial use
  requires a license. The company name has changed to Mayorana, and the
  copyright notice has been updated accordingly.

## [0.1.18] - 2026-10-06

### Added

- Splitter now shares anonymous usage statistics: whether it is installed and
  opened, its version and your operating system. It is on by default; a note at
  the bottom of the window tells you once, and nothing is sent before you have
  seen it. **Turn off** there stops it for good and deletes anything not yet
  sent. Your files, data and accounts are never part of it. It also stays off if
  `DO_NOT_TRACK`, `DISABLE_UPDATE_CHECK` or `MAYORANA_NO_TELEMETRY` is set.

## [0.1.17] - 2026-10-05

### Changed

- Packaging only — no user-visible change.

## [0.1.16] - 2026-10-05

### Added

- Videos without sound can be cut. They open like any other video, with the
  picture shown and a flat timeline, and you place every split yourself with
  **M**, since there is no silence to find. Their clips, and the copy that
  **Apply cuts** makes, are picture only.
- **URL…** with **Video** now downloads videos posted without sound, as is
  common on X, instead of failing. Asking for **Audio** on such a video now
  says to pick **Video** instead.

## [0.1.15] - 2026-09-28

### Changed

- Packaging only — no user-visible change.

## [0.1.14] - 2026-09-27

### Added

- The version you are running now shows next to the Splitter name at the top
  of the sidebar and in the window title, so it is at hand when you report a
  problem or check whether an update installed.
- A **Get Pro…** button at the top of the file list opens the licence window,
  where you buy Splitter Pro or paste your key. It reads **Pro ✓** once your
  licence is active. Clicking **Splitter** still opens the same window.

## [0.1.13] - 2026-09-27

### Changed

- Without Splitter Pro, an export now writes the first 10 tracks of each
  recording. Splitting, reviewing and naming are not limited: when a recording
  has more tracks, the export bar says how many and **Splitter Pro exports all**
  opens the licence window. A vinyl side, an EP or a short set still fits in
  the free version; with Pro, every track is exported, as before.

## [0.1.12] - 2026-09-27

### Added

- Splitter Pro licences. Click **Splitter** at the top of the file list to
  paste the licence key from your purchase email. The key is checked on your
  computer, with no account and nothing sent anywhere, and a **Pro** badge
  shows once it is active. **Remove from this computer** frees it for another
  one.

## [0.1.11] - 2026-09-27

### Changed

- Packaging only — no user-visible change.

## [0.1.10] - 2026-09-25

### Changed

- Packaging only — no user-visible change.

## [0.1.9] - 2026-09-24

### Changed

- Packaging only — no user-visible change.

## [0.1.8] - 2026-09-24

### Changed

- Packaging only — no user-visible change.

## [0.1.7] - 2026-09-24

### Fixed

- The video preview works on Windows web views that refuse to load media from
  a local address. When that happens the preview falls back to the app's own
  channel, so the picture still shows instead of an error.

### Changed

- The notes for every release, with its download, are now published at
  [mayorana.ch/en/apps/splitter/releases](https://mayorana.ch/en/apps/splitter/releases).

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
