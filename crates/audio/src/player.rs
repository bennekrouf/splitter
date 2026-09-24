//! Playback engine.
//!
//! A dedicated thread owns the cpal stream (not `Send` on macOS) and the decoder. It decodes
//! ahead into a ring buffer, already converted to the device's channel count and rate; the
//! cpal callback only copies out of the ring. The playhead is published through atomics, so
//! the UI can poll it at 60 fps without touching the audio thread.

use crate::mp3index::Mp3Index;
use crate::ring::Ring;
use crate::source::{open_source, MemPcm, PcmSource};
use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

enum Cmd {
    Load { path: PathBuf, mp3: Option<Arc<Mp3Index>>, total: u64 },
    Unload,
    Play,
    Pause,
    Toggle,
    Seek(u64),
    PlayRange(u64, u64),
    SetAlt(Option<AltPcm>),
    UseAlt(bool),
    Loop(Option<(u64, u64)>),
}

/// Alternative audio for part of the recording (an encoded preview), aligned to source frames.
#[derive(Clone)]
pub struct AltPcm {
    pub pcm: Arc<Vec<f32>>,
    pub start: u64,
}

#[derive(Default)]
struct Shared {
    loaded: AtomicBool,
    playing: AtomicBool,
    /// Source frame at the head of the ring when `consumed` was last reset.
    base: AtomicU64,
    /// Device frames the callback has played since `base`.
    consumed: AtomicU64,
    /// Source frames per device frame, as f64 bits.
    ratio: AtomicU64,
    total: AtomicU64,
    /// The decoder hit end of stream; once the ring drains, playback stops.
    eos: AtomicBool,
    /// Set by the engine, cleared by the callback after it empties the ring.
    flush: AtomicBool,
    /// Pause when the playhead reaches this source frame (`u64::MAX` = never).
    stop_at: AtomicU64,
    /// Playing the alternative (encoded) audio instead of the file.
    using_alt: AtomicBool,
    error: Mutex<Option<String>>,
}

#[derive(Clone)]
pub struct Player {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
}

impl Player {
    pub fn spawn() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared { stop_at: AtomicU64::new(u64::MAX), ..Default::default() });
        let s = shared.clone();
        std::thread::Builder::new()
            .name("splitter-audio".into())
            .spawn(move || Engine::new(s).run(rx))
            .expect("spawn audio thread");
        Self { tx, shared }
    }

    pub fn load(&self, path: PathBuf, mp3: Option<Arc<Mp3Index>>, total: u64) {
        let _ = self.tx.send(Cmd::Load { path, mp3, total });
    }
    pub fn unload(&self) {
        let _ = self.tx.send(Cmd::Unload);
    }
    pub fn play(&self) {
        let _ = self.tx.send(Cmd::Play);
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }
    pub fn toggle(&self) {
        let _ = self.tx.send(Cmd::Toggle);
    }
    pub fn seek(&self, frame: u64) {
        let _ = self.tx.send(Cmd::Seek(frame));
    }
    /// Play `[from, to)` and pause at `to`, leaving the playhead there.
    pub fn play_range(&self, from: u64, to: u64) {
        let _ = self.tx.send(Cmd::PlayRange(from, to));
    }
    /// Provide (or remove) alternative audio for A/B comparison.
    pub fn set_alt(&self, alt: Option<AltPcm>) {
        let _ = self.tx.send(Cmd::SetAlt(alt));
    }
    /// Switch between the file and the alternative audio at the current position.
    pub fn use_alt(&self, on: bool) {
        let _ = self.tx.send(Cmd::UseAlt(on));
    }
    /// Loop playback over `[from, to)`.
    pub fn set_loop(&self, range: Option<(u64, u64)>) {
        let _ = self.tx.send(Cmd::Loop(range));
    }

    pub fn is_using_alt(&self) -> bool {
        self.shared.using_alt.load(Relaxed)
    }

    pub fn is_loaded(&self) -> bool {
        self.shared.loaded.load(Relaxed)
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Relaxed)
    }

    /// Current playhead in source sample frames.
    pub fn position(&self) -> u64 {
        let s = &self.shared;
        let ratio = f64::from_bits(s.ratio.load(Relaxed));
        let pos = s.base.load(Relaxed) + (s.consumed.load(Relaxed) as f64 * ratio) as u64;
        pos.min(s.total.load(Relaxed))
    }

    pub fn take_error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().take()
    }
}

struct Loaded {
    source: Box<dyn PcmSource>,
    /// The file's source while the alternative audio is playing.
    stashed: Option<Box<dyn PcmSource>>,
    alt: Option<AltPcm>,
    loop_range: Option<(u64, u64)>,
    _stream: cpal::Stream,
    ring: Arc<Ring>,
    dev_channels: usize,
    resampler: Resampler,
    src_buf: Vec<f32>,
    pending: Vec<f32>,
    pending_at: usize,
    eos: bool,
}

struct Engine {
    shared: Arc<Shared>,
    cur: Option<Loaded>,
}

impl Engine {
    fn new(shared: Arc<Shared>) -> Self {
        Self { shared, cur: None }
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            let wait = if self.wants_data() { Duration::ZERO } else { Duration::from_millis(5) };
            match rx.recv_timeout(wait) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            while let Ok(cmd) = rx.try_recv() {
                self.handle(cmd);
            }
            self.fill();
            self.check_loop();
        }
    }

    fn position(&self) -> u64 {
        let s = &self.shared;
        let ratio = f64::from_bits(s.ratio.load(Relaxed));
        s.base.load(Relaxed) + (s.consumed.load(Relaxed) as f64 * ratio) as u64
    }

    fn check_loop(&mut self) {
        let Some((a, b)) = self.cur.as_ref().and_then(|c| c.loop_range) else { return };
        // Stopped because everything was played (not paused with audio still buffered).
        let ended = self.shared.stop_at.load(Relaxed) == u64::MAX
            && !self.shared.playing.load(Relaxed)
            && self.cur.as_ref().is_some_and(|c| c.eos && c.ring.len() == 0 && c.pending_at >= c.pending.len());
        if ended {
            // The in-memory preview ran out at the window's end: wrap around and keep playing.
            self.seek(a);
            self.shared.playing.store(true, Relaxed);
        } else if self.shared.playing.load(Relaxed) && self.position() >= b {
            self.seek(a);
        }
    }

    /// Swap between the file and the alternative audio, continuing from the same position.
    fn use_alt(&mut self, on: bool) {
        let pos = self.position();
        let s = self.shared.clone();
        let Some(cur) = &mut self.cur else { return };
        match (on, cur.stashed.is_some()) {
            (true, false) => {
                let Some(alt) = cur.alt.clone() else { return };
                let mem = MemPcm::new(alt.pcm, cur.source.channels(), cur.source.sample_rate(), alt.start);
                cur.stashed = Some(std::mem::replace(&mut cur.source, Box::new(mem)));
            }
            (false, true) => {
                cur.source = cur.stashed.take().unwrap();
            }
            _ => return,
        }
        s.using_alt.store(on, Relaxed);
        self.seek(pos);
    }

    fn report(&self, e: anyhow::Error) {
        eprintln!("splitter audio: {e:#}");
        *self.shared.error.lock().unwrap() = Some(format!("{e:#}"));
    }

    fn handle(&mut self, cmd: Cmd) {
        let s = self.shared.clone();
        match cmd {
            Cmd::Load { path, mp3, total } => {
                self.unload();
                match self.load(path, mp3, total) {
                    Ok(l) => {
                        self.cur = Some(l);
                        s.loaded.store(true, Relaxed);
                    }
                    Err(e) => self.report(e),
                }
            }
            Cmd::Unload => self.unload(),
            Cmd::Play => {
                s.stop_at.store(u64::MAX, Relaxed);
                self.play();
            }
            Cmd::Pause => s.playing.store(false, Relaxed),
            Cmd::Toggle => {
                s.stop_at.store(u64::MAX, Relaxed);
                if s.playing.load(Relaxed) {
                    s.playing.store(false, Relaxed);
                } else {
                    self.play();
                }
            }
            Cmd::Seek(frame) => {
                s.stop_at.store(u64::MAX, Relaxed);
                self.seek(frame);
            }
            Cmd::PlayRange(from, to) => {
                self.seek(from);
                s.stop_at.store(to, Relaxed);
                self.play();
            }
            Cmd::SetAlt(alt) => {
                if alt.is_none() {
                    self.use_alt(false);
                }
                if let Some(cur) = &mut self.cur {
                    cur.alt = alt;
                }
            }
            Cmd::UseAlt(on) => self.use_alt(on),
            Cmd::Loop(range) => {
                if let Some(cur) = &mut self.cur {
                    cur.loop_range = range;
                }
            }
        }
    }

    fn unload(&mut self) {
        let s = &self.shared;
        s.playing.store(false, Relaxed);
        s.loaded.store(false, Relaxed);
        self.cur = None; // drops the stream
        s.using_alt.store(false, Relaxed);
        s.base.store(0, Relaxed);
        s.consumed.store(0, Relaxed);
        s.total.store(0, Relaxed);
        s.eos.store(false, Relaxed);
        s.flush.store(false, Relaxed);
    }

    fn play(&mut self) {
        let Some(cur) = &self.cur else { return };
        let at_end = cur.eos && cur.ring.len() == 0 && cur.pending_at >= cur.pending.len();
        if at_end && self.shared.stop_at.load(Relaxed) == u64::MAX {
            self.seek(0);
        }
        self.shared.playing.store(true, Relaxed);
    }

    fn seek(&mut self, frame: u64) {
        let s = self.shared.clone();
        let Some(cur) = &mut self.cur else { return };
        let frame = frame.min(s.total.load(Relaxed));

        // Stop producing, have the callback empty the ring, then restart from `frame`.
        s.consumed.store(0, Relaxed);
        s.base.store(frame, Relaxed);
        s.flush.store(true, Release);
        let deadline = Instant::now() + Duration::from_millis(250);
        while s.flush.load(Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_micros(500));
        }
        s.flush.store(false, Relaxed);
        cur.ring.clear(); // callback is idle between buffers; harmless if it already did
        s.consumed.store(0, Relaxed);

        cur.pending.clear();
        cur.pending_at = 0;
        cur.resampler.reset();
        cur.eos = false;
        s.eos.store(false, Relaxed);
        if let Err(e) = cur.source.seek(frame) {
            cur.eos = true;
            s.eos.store(true, Relaxed);
            self.report(e.context("seek failed"));
        }
    }

    fn wants_data(&self) -> bool {
        self.cur.as_ref().is_some_and(|c| !c.eos && c.ring.free() >= c.ring_chunk())
    }

    fn fill(&mut self) {
        let s = self.shared.clone();
        let Some(cur) = &mut self.cur else { return };
        loop {
            if cur.pending_at < cur.pending.len() {
                let n = cur.ring.push(&cur.pending[cur.pending_at..], cur.dev_channels);
                cur.pending_at += n;
                if cur.pending_at < cur.pending.len() {
                    return; // ring full
                }
                continue;
            }
            if cur.eos || cur.ring.free() < cur.ring_chunk() {
                return;
            }
            cur.src_buf.clear();
            match cur.source.read(&mut cur.src_buf) {
                Ok(true) => {
                    cur.pending.clear();
                    cur.pending_at = 0;
                    let src_ch = cur.source.channels();
                    cur.resampler.process(&cur.src_buf, src_ch, cur.dev_channels, &mut cur.pending);
                }
                Ok(false) => {
                    cur.eos = true;
                    s.eos.store(true, Relaxed);
                }
                Err(e) => {
                    cur.eos = true;
                    s.eos.store(true, Relaxed);
                    eprintln!("splitter audio: decode error: {e:#}");
                }
            }
        }
    }

    fn load(&mut self, path: PathBuf, mp3: Option<Arc<Mp3Index>>, total: u64) -> Result<Loaded> {
        let source = open_source(&path, mp3)?;
        let src_rate = source.sample_rate();

        let device = cpal::default_host().default_output_device().ok_or_else(|| anyhow!("no audio output device"))?;
        let config = pick_config(&device, src_rate)?;
        let dev_rate = config.sample_rate.0;
        let dev_channels = config.channels as usize;

        // ~0.3 s of device audio buffered ahead.
        let ring = Arc::new(Ring::new(dev_rate as usize * dev_channels * 3 / 10));
        let s = self.shared.clone();
        s.total.store(total, Relaxed);
        s.base.store(0, Relaxed);
        s.consumed.store(0, Relaxed);
        s.eos.store(false, Relaxed);
        s.ratio.store((src_rate as f64 / dev_rate as f64).to_bits(), Relaxed);

        let cb_ring = ring.clone();
        let cb = self.shared.clone();
        let stream = device
            .build_output_stream(
                &config,
                move |out: &mut [f32], _| {
                    if cb.flush.load(Acquire) {
                        cb_ring.clear();
                        cb.consumed.store(0, Relaxed);
                        cb.flush.store(false, Release);
                    }
                    if !cb.playing.load(Relaxed) {
                        out.fill(0.0);
                        return;
                    }
                    // Don't play past `stop_at`.
                    let mut want = out.len();
                    let stop = cb.stop_at.load(Relaxed);
                    if stop != u64::MAX {
                        let ratio = f64::from_bits(cb.ratio.load(Relaxed));
                        let pos = cb.base.load(Relaxed) as f64 + cb.consumed.load(Relaxed) as f64 * ratio;
                        let left = ((stop as f64 - pos).max(0.0) / ratio).ceil() as usize;
                        want = want.min(left * dev_channels);
                    }
                    let n = cb_ring.pop(&mut out[..want]);
                    out[n..].fill(0.0);
                    cb.consumed.fetch_add((n / dev_channels) as u64, Relaxed);
                    if want < out.len() && n == want {
                        cb.playing.store(false, Relaxed);
                        cb.stop_at.store(u64::MAX, Relaxed);
                    } else if n < out.len() && cb.eos.load(Relaxed) && cb_ring.len() == 0 {
                        cb.playing.store(false, Relaxed);
                    }
                },
                |e| eprintln!("splitter audio: stream error: {e}"),
                None,
            )
            .context("opening audio output")?;
        stream.play().context("starting audio output")?;

        Ok(Loaded {
            source,
            stashed: None,
            alt: None,
            loop_range: None,
            _stream: stream,
            ring,
            dev_channels,
            resampler: Resampler::new(src_rate as f64 / dev_rate as f64),
            src_buf: Vec::with_capacity(8192),
            pending: Vec::with_capacity(8192),
            pending_at: 0,
            eos: false,
        })
    }
}

impl Loaded {
    fn ring_chunk(&self) -> usize {
        4096 * self.dev_channels
    }
}

/// Prefer an f32 config at the file's own rate (no resampling); otherwise the device default.
fn pick_config(device: &cpal::Device, rate: u32) -> Result<cpal::StreamConfig> {
    let exact = device
        .supported_output_configs()
        .context("querying output configs")?
        .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
        .filter(|c| c.min_sample_rate().0 <= rate && rate <= c.max_sample_rate().0)
        .min_by_key(|c| (c.channels() as i32 - 2).abs())
        .map(|c| c.with_sample_rate(cpal::SampleRate(rate)));
    let chosen = match exact {
        Some(c) => c,
        None => {
            let d = device.default_output_config().context("no default output config")?;
            if d.sample_format() != cpal::SampleFormat::F32 {
                return Err(anyhow!("output device has no f32 format"));
            }
            d
        }
    };
    Ok(chosen.config())
}

/// Channel mapping plus linear-interpolation rate conversion. Good enough for monitoring;
/// exports never go through here.
struct Resampler {
    ratio: f64,
    t: f64,
    prev: Vec<f32>,
    frame: Vec<f32>,
}

impl Resampler {
    fn new(ratio: f64) -> Self {
        Self { ratio, t: 0.0, prev: Vec::new(), frame: Vec::new() }
    }

    fn reset(&mut self) {
        self.t = 0.0;
        self.prev.clear();
    }

    fn map(src: &[f32], dev_ch: usize, out: &mut Vec<f32>) {
        for d in 0..dev_ch {
            out.push(match src.len() {
                1 => src[0],
                n if d < n => src[d],
                _ => 0.0,
            });
        }
    }

    fn process(&mut self, input: &[f32], src_ch: usize, dev_ch: usize, out: &mut Vec<f32>) {
        let frames = input.chunks_exact(src_ch);
        if (self.ratio - 1.0).abs() < 1e-9 {
            for f in frames {
                Self::map(f, dev_ch, out);
            }
            return;
        }
        // Frame 0 is `prev` (last frame of the previous chunk), then this chunk's frames.
        let mut src: Vec<&[f32]> = Vec::with_capacity(input.len() / src_ch + 1);
        let prev = std::mem::take(&mut self.prev);
        if !prev.is_empty() {
            src.push(&prev);
        }
        src.extend(frames);
        if src.len() < 2 {
            self.prev = src.first().map(|f| f.to_vec()).unwrap_or_default();
            return;
        }
        let last = (src.len() - 1) as f64;
        while self.t < last {
            let i = self.t as usize;
            let f = (self.t - i as f64) as f32;
            self.frame.clear();
            let (a, b) = (src[i], src[i + 1]);
            self.frame.extend(a.iter().zip(b).map(|(x, y)| x * (1.0 - f) + y * f));
            Self::map(&self.frame, dev_ch, out);
            self.t += self.ratio;
        }
        self.t -= last;
        self.prev = src[src.len() - 1].to_vec();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_rate() {
        let mut r = Resampler::new(44100.0 / 48000.0);
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&[0.5f32; 441 * 2], 2, 2, &mut out);
        }
        let frames = out.len() / 2;
        assert!((frames as i64 - 48000).abs() < 4, "{frames}");
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn mono_is_duplicated() {
        let mut r = Resampler::new(1.0);
        let mut out = Vec::new();
        r.process(&[0.1, 0.2], 1, 2, &mut out);
        assert_eq!(out, vec![0.1, 0.1, 0.2, 0.2]);
    }
}
