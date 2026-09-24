# Splitter

Keyboard-driven tool for splitting long MP3/WAV recordings into tracks. Rust + Dioxus desktop.
Videos (MP4, M4V, MOV with AAC audio) open too: they're split on their sound and exported as
video clips.

## Layout

```
crates/core    project model (queue, status), time formatting — no audio, no UI
crates/audio   MP3 frame index, scan (peaks + loudness, cached), seekable sources, playback engine
crates/app     Dioxus desktop UI
```

## Run

```bash
cargo run -p splitter -- path/to/folder
```

Or start without an argument and use **Open folder…** (⌘O).

In the file list, **🗑** (on hover) moves a recording to the system Trash after a confirmation
(Enter confirms, Esc cancels); its splits and titles are dropped, exported tracks are left
alone. Drag the list's right edge to make it wider, or double-click the edge to fit the longest
file name; the width is remembered.

### Videos

MP4, M4V and MOV files are listed next to the recordings. A file is split either as audio or as
video, never both: a video shows its picture where a recording shows its overview waveform
(click it to play or pause), and a plain timeline instead of the detail waveform, with the same
splits, silences and track names. Its sound is scanned, played and split like any recording's
(silence detection, review keys, preview cuts, loudness).

The picture is played muted by the system's web view, from a small HTTP server on 127.0.0.1
that serves only the selected file under a random token, and follows the audio player's
playhead: the exact frame while paused, within a few tens of ms while playing. A web view that
can't decode the video (e.g. HEVC on Windows without its extension, or Linux without
GStreamer's H.264 plugin) shows a note; the sound works as usual.

**Export** writes one MP4 clip per kept track (`live/01 - Intro.mp4`, …) with title, album and
track number, cut exactly on the splits: the clips are re-encoded by ffmpeg (H.264 at high,
standard or small quality, AAC 192 kbps), since a copy could only cut on keyframes, often seconds
away. Loudness normalization applies to the sound. **Apply cuts** writes `live (cleaned).mp4`
the same way. ffmpeg is fetched on first use (about 30 MB, SHA-256 checked, into the same
`tools/` folder as yt-dlp): a static GPL build with x264, from
[yt-dlp's builds](https://github.com/yt-dlp/FFmpeg-Builds) on Windows and
[Martin Riedl's](https://ffmpeg.martin-riedl.de) on macOS and Linux; it's never bundled with the
app.

Symphonia decodes the AAC encoder's priming samples, which ffmpeg and the web views skip as the
file's edit list says; `splitter_audio::mp4` reads that list so splits, the picture and the
clips line up to the sample. Only AAC-LC sound is supported (symphonia's decoder): HE-AAC and
multichannel AAC, and MKV/WebM files, don't open yet.

### From a YouTube link

**URL…** (⌘U) downloads a video's audio and opens it from `~/Music/Splitter/Downloads`. Pick
**Video** in the dialog to get the video instead: picture (H.264, up to 1080p) and sound are
downloaded separately, as YouTube serves them, and merged into one MP4 with ffmpeg. Nothing
needs installing: on first use the app fetches [yt-dlp](https://github.com/yt-dlp/yt-dlp)'s
standalone build (Python included) and [Deno](https://deno.com) (the JavaScript runtime yt-dlp
needs for YouTube) into its data folder (`tools/`), checking both against their published SHA-256
sums. yt-dlp updates itself at most once a day. For audio it fetches the AAC (.m4a) or MP3
stream, so no ffmpeg: AAC is decoded to WAV with symphonia; a video download fetches ffmpeg
first (see Videos). Works on macOS, Windows and Linux (x86_64/arm64).
The download tests need the network: `cargo test -p splitter -- --ignored download`, and
`--ignored ffmpeg` installs ffmpeg, after which the video export tests run against it.

Generate a 60-minute test recording (tracks separated by 2 s gaps):

```bash
cargo run -p splitter-audio --release --example make_test_recording -- testdata 60
```

Export a folder from its cutlist without the UI, checking every track's length:

```bash
cargo run -p splitter --release --example export_cutlist -- testdata --out /tmp/out --verify \
    --profile mp3-v2 --normalize album:-14
```

Scan and seek timings on any file:

```bash
cargo run -p splitter-audio --release --example bench -- testdata/live-set-60min.mp3
```

## Keys

**Reviewing splits**

| Key | Action |
|---|---|
| Tab / ⇧Tab | next / previous split, playing 3 s on each side |
| Enter | keep the split, go to the next one still to review |
| ⌫ | delete the split, go to the next one still to review |
| , / . | nudge the split 10 ms (⇧ 100 ms) |
| S | snap the split to the quietest point within ±0.5 s |
| G | keep the silence at the split (or under the playhead) in the export, or cut it again |
| M | add a split at the playhead |
| C | listen across the split again |
| drag a marker | move a split |
| ⌘Z / ⇧⌘Z | undo / redo |
| ⌘Enter | mark the file done, open the next one |
| Esc | deselect |

**Tracks and export**

| Key | Action |
|---|---|
| T | type the current track's title (Enter saves and moves to the next; Tab does too, and on an empty field accepts the suggested "Track N"; ⇧Tab goes back; Esc cancels). Titles show on the waveform at each track's start |
| X | cut the current part (leave it out of the export), or bring it back |
| R | rename the recording (✎ next to its name): the file on disk and its export folder |
| A | A/B: loop 12 s from the playhead, then switch between original and the export format |
| Esc | stop A/B |
| F | flag the file to come back to |
| ⌘E | export the kept tracks (queued) |
| ⇧⌘E | export every file marked done |

The *current part* is highlighted in blue on the waveform ("Track N · X to cut"): right after
**M** it's the part that mark closes, so **M** at 3:00, **M** at 4:00, **X** cuts 3:00–4:00.
Otherwise it's the one after the selected split, or the one under the playhead. Cut parts are
red and labelled "✂ cut"; X on one brings it back.
**Paste tracklist…** takes one title per line (numbering, timestamps and durations are
stripped) and gives them to the kept tracks in order.

**Playback and navigation**

| Key | Action |
|---|---|
| Space | play / pause |
| P | **preview cuts**: play only what the export keeps, jumping over cut silence and left-out tracks |
| ← / → | ±5 s (⇧ 0.5 s, ⌥ 10 ms) |
| ↑ / ↓ | previous / next file |
| + / − | zoom the detail view |
| ⌘ + scroll | zoom; scroll pans |
| Home / End | start / end |
| click / drag | seek (overview centres the detail view) |

## Splits and the cutlist

When a file is first analysed, silence detection suggests a split in the middle of every quiet
stretch (below −45 dB for at least 1.5 s by default, ignoring lead-in/tail silence and keeping at
least 30 s between suggestions). Both settings can be changed per file in the review bar;
**Re-detect** replaces the unreviewed suggestions and keeps every confirmed split.

Detection also tags every quiet stretch as *silence* (hatched red on the waveforms), including
the lead-in and the tail. Where a silence touches a track's edge (around a split, or at the
start/end of the recording) it is cut from the export by default: tracks start just before the
music and stop just after it, keeping 0.25 s of the silence on each side so quiet attacks and
decays survive. **G** keeps a silence (green) or cuts it again. Silence inside a track (a
suggestion you deleted) is shown faintly and never cut, and a track that is nothing but cut
silence is skipped. Splits don't move: only what gets exported changes, and the track list
shows each track's length after the cut.

To check the result by ear, turn on **Preview cuts** (P) and play from the start: you hear the
exported tracks back to back, and nothing is written to disk. The player stops decoding at the
start of each skipped part and jumps past it, so none of the skipped audio is played. Edits
apply straight away, including while it plays.

When it sounds right, **✂ Apply cuts** (top right) writes a copy without the cut parts,
`live-set (cleaned).wav` next to the original, and opens it as a new entry in the file list. Its
waveform and track list only show what was kept, with the splits already confirmed and the
titles carried over. The original is never changed, so it's still in the list if you need it,
and **⌘Z** right after applying removes the copy and goes back to the original. The copy is
always WAV: a byte-for-byte copy of a WAV source, or the decoded audio of an MP3 as 16-bit PCM,
because MP3 frames can't be cut out of the middle without clicks. That's about 600 MB per hour.

All edits are saved to `splitter.cutlist.json` in the recordings folder, right after each change.
Split positions are sample frames at the file's own rate, keyed by file name. The review loop
itself (`step_from`, `confirm_and_next`, `delete_and_next`, undo) lives in
`crates/core/src/edit.rs` and is unit-tested by replaying key sequences.

## Export

Tracks go to a folder named after the recording, next to it: `live-set/01 - Intro.mp3`, … The
name template (`{nn} - {title}` by default; also `{n}`, `{source}`) is stored in the cutlist.
Dropped tracks are skipped and the rest are numbered consecutively. Existing files with the same
names are replaced; files from an earlier export with different names are left alone.

- **MP3** is never re-encoded. The original frames are copied, with two extra frames in front to
  warm the decoder up and one silent *carrier* frame holding the bit-reservoir bytes those frames
  need. An Info/LAME header records the encoder delay and padding, so gapless decoders (ffmpeg,
  symphonia, iTunes, foobar2000…) play exactly `[start, end)`. Players that ignore the LAME header
  play up to ~0.1 s extra at each end. A track starting at 0 loses the first 529 samples (12 ms of
  encoder lead-in). Files get ID3v2 title, album (the recording's name) and track number.
- **WAV** tracks are a straight byte copy of the sample data with the original format chunk.

### Formats

The export bar picks the format (saved in the cutlist) and shows an estimated total size
(Original is exact; the others are typical averages) plus warnings, e.g. re-encoding an MP3, a
bitrate above the source's, or FLAC/WAV from an MP3 (bigger files, same quality).

| Format | How |
|---|---|
| Original | lossless copy, as above |
| MP3 VBR V0 / V2 / V4, CBR 320–128 | LAME (`-q 2`), with LAME's own gapless header; ID3v2 tags |
| FLAC | flacenc, 16-bit (24-bit for >16-bit sources); Vorbis comment tags |
| WAV 16-bit | PCM, no tags |

Re-encoded tracks are cut from the decoded audio, so they start and end exactly on the split.
**A/B** encodes the 12 s window with the chosen format in the background, decodes it back and
loops it; **A** swaps between original and encoded at the same position. LAME is LGPL and is
compiled in statically; keep that in mind if you ever distribute the app.

Tests decode every exported track and compare it sample by sample with the source, for CBR,
VBR and 32 kbps MP3 (where the reservoir spans many frames) and WAV.

### Queue

Exports run one file at a time on a background thread. ⌘E queues the current file, **Export N
done files** in the sidebar (⇧⌘E) queues every file marked done (⌘Enter). The sidebar shows
each file's progress; **Cancel** stops after the current track and drops the rest. A finished
file is marked *exported*.

### Loudness

After the files are scanned, each one's loudness is measured in the background (EBU R128: 400 ms
block loudness every 100 ms, and true peak; ~4 s per hour of audio, cached). Track and set
loudness are derived from those blocks instantly, so the track list's **LUFS** and **peak**
columns follow every split change. Peaks above −1 dBTP are shown in red.

- Every MP3/FLAC export gets **ReplayGain 2.0** tags (track, and album for the exported set).
- Re-encoded exports can be **normalized**: *whole recording* (one gain for all tracks, keeping
  the dynamics between songs; usually right for a live set) or *each track*, to −14 or −16 LUFS.
  The gain never pushes the true peak above −1 dBTP. Original (byte copy) can't change level,
  so it only gets the tags.

## Building and releasing

Same setup as GitAgent and the ais-* apps.

- **Icon**: `crates/app/assets/icon.svg` is the source; `icon.png` (1024 px) is rendered from it
  (`rsvg-convert -w 1024 -h 1024 icon.svg -o icon.png`). The app sets it as the window icon at
  runtime; release CI turns it into `icon.icns` (macOS) and `icon.ico` (Windows, embedded in
  the .exe by `crates/app/build.rs`), and ships the PNG for the Linux launcher.
- **CI** (`.github/workflows/ci.yml`, on pull requests): `cargo fmt --check`, tests, and clippy
  with warnings as errors.
- **Release** (`.github/workflows/release.yml`, on a `v*` tag or run by hand from the Actions
  tab): a signed and notarized `.dmg` for Apple Silicon, a signed Inno Setup installer for
  Windows (`installer/installer.iss`), and a Linux x86_64 tarball with `scripts/setup-linux.sh`
  (installs WebKitGTK and ALSA, and adds a launcher). Artifacts are published to
  `mayorana.ch/downloads/splitter/{<tag>,latest}/` with a `latest.json`.
- **Cutting a release**: `./scripts/release.sh` (patch bump, or `--minor`, `--major`, a version,
  or `--dry-run`) bumps the workspace version, tags and pushes.

Signing and publishing need the same repository secrets as the other apps: the seven macOS
ones (`MACOS_*`, `KEYCHAIN_PASSWORD`, `APP_STORE_CONNECT_*`), the six Azure Trusted Signing ones
for Windows, and `DIST_SSH_*` for the upload. Without the macOS secrets the release skips the
.dmg; without the Windows ones the installer ships unsigned.

## How it works

- **Scan (once per file, cached in `~/Library/Caches/splitter`)**: builds a byte-offset index of every
  MP3 frame, min/max peaks at 5 zoom levels, and 50 ms RMS windows (used for silence detection).
  About 2 s for an hour of VBR MP3.
- **Last folder**: reopened on start when no folder is given (`~/Library/Application Support/splitter`).
- **Seeking**: MP3 seeks reopen the demuxer at the indexed frame 2 frames before the target and
  discard up to the exact sample. The tests check that this is sample-identical to a straight decode on
  CBR, VBR and WAV.
- **Playback**: an audio thread owns the cpal stream and decodes ahead into a lock-free ring. The
  playhead is published via atomics and polled by the UI at 60 fps. Only the playhead components
  re-render per frame; the waveform SVG re-renders only when the view changes.
