# Splitter

Keyboard-driven tool for splitting long MP3/WAV recordings into tracks. Rust + Dioxus desktop.

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

### From a YouTube link

**URL…** (⌘U) downloads a video's audio and opens it from `~/Music/Splitter/Downloads`. Nothing
needs installing: on first use the app fetches [yt-dlp](https://github.com/yt-dlp/yt-dlp)'s
standalone build (Python included) and [Deno](https://deno.com) (the JavaScript runtime yt-dlp
needs for YouTube) into its data folder (`tools/`), checking both against their published SHA-256
sums. yt-dlp updates itself at most once a day. It fetches the AAC (.m4a) or MP3 stream, so no
ffmpeg: AAC is decoded to WAV with symphonia. Works on macOS, Windows and Linux (x86_64/arm64).
The download test needs the network: `cargo test -p splitter -- --ignored download`.

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
| T | type the current track's title (Enter saves and moves to the next, Esc cancels) |
| X | leave the current track out of the export, or bring it back |
| A | A/B: loop 12 s from the playhead, then switch between original and the export format |
| Esc | stop A/B |
| F | flag the file to come back to |
| ⌘E | export the kept tracks (queued) |
| ⇧⌘E | export every file marked done |

The *current track* is the one after the selected split, or the one under the playhead.
**Paste tracklist…** takes one title per line (numbering, timestamps and durations are
stripped) and gives them to the kept tracks in order.

**Playback and navigation**

| Key | Action |
|---|---|
| Space | play / pause |
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
