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

Generate a 60-minute test recording (tracks separated by 2 s gaps):

```bash
cargo run -p splitter-audio --release --example make_test_recording -- testdata 60
```

Export a folder from its cutlist without the UI, checking every track's length:

```bash
cargo run -p splitter --release --example export_cutlist -- testdata --out /tmp/out --verify --profile mp3-v2
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
| ⌘E | export the kept tracks |

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

## How it works

- **Scan (once per file, cached in `~/Library/Caches/splitter`)**: builds a byte-offset index of every
  MP3 frame, min/max peaks at 5 zoom levels, and 50 ms loudness windows (used for silence detection
  in step 2). About 2 s for an hour of VBR MP3.
- **Seeking**: MP3 seeks reopen the demuxer at the indexed frame 2 frames before the target and
  discard up to the exact sample. The tests check that this is sample-identical to a straight decode on
  CBR, VBR and WAV.
- **Playback**: an audio thread owns the cpal stream and decodes ahead into a lock-free ring. The
  playhead is published via atomics and polled by the UI at 60 fps. Only the playhead components
  re-render per frame; the waveform SVG re-renders only when the view changes.
