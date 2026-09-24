//! The export queue: recordings are exported one after another on a worker thread, with
//! progress per file and a cancel that stops after the current track.

use crate::state::{key_of, App, ScanState};
use crate::tools;
use crate::video_export::{export_clip, Clip, Timeline};
use crossbeam_channel::{Receiver, Sender};
use dioxus::prelude::*;
use splitter_audio::export::{can_copy, export, ExportJob, Tags};
use splitter_audio::loudness::{apply_to_jobs, load_or_analyze, LoudnessMap};
use splitter_audio::{Bitrate, Scan};
use splitter_core::export::{plan, Normalize, Profile, SourceFacts, VideoProfile};
use splitter_core::Status;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Where one recording stands in the export queue.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportStatus {
    Queued,
    /// Measuring loudness before a normalized export.
    Measuring,
    /// Fetching ffmpeg before the first video export.
    Installing(f32),
    Running {
        done: usize,
        total: usize,
        /// How far into the current track (video clips take a while to encode).
        part: f32,
    },
    Finished {
        dir: PathBuf,
        count: usize,
    },
    Failed(String),
    Cancelled,
}

impl ExportStatus {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            ExportStatus::Queued | ExportStatus::Measuring | ExportStatus::Installing(_) | ExportStatus::Running { .. }
        )
    }

    /// Overall progress while running.
    pub fn fraction(&self) -> Option<f64> {
        match self {
            ExportStatus::Running { done, total, part } => Some((*done as f64 + *part as f64) / (*total).max(1) as f64),
            _ => None,
        }
    }
}

struct Task {
    key: String,
    generation: u64,
    source: PathBuf,
    scan: Arc<Scan>,
    jobs: Vec<ExportJob>,
    dir: PathBuf,
    profile: Profile,
    normalize: Normalize,
    loudness: Option<Arc<LoudnessMap>>,
    /// For a video: clips with ffmpeg in this format, instead of `profile`.
    video: Option<VideoProfile>,
}

enum Event {
    Status(String, ExportStatus),
    /// Loudness measured for a normalized export; worth keeping.
    Loudness(PathBuf, Arc<LoudnessMap>),
}

/// Handle to the export worker.
#[derive(Clone)]
pub struct Exporter {
    tx: Sender<Task>,
    events: Receiver<Event>,
    /// Tasks from an older generation are skipped: bumping it cancels everything queued.
    generation: Arc<AtomicU64>,
}

impl Exporter {
    pub fn spawn() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Task>();
        let (etx, events) = crossbeam_channel::unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let g = generation.clone();
        std::thread::Builder::new()
            .name("splitter-export".into())
            .spawn(move || {
                for task in rx {
                    run(task, &g, &etx);
                }
            })
            .expect("spawn export thread");
        Self { tx, events, generation }
    }
}

fn run(mut task: Task, generation: &AtomicU64, events: &Sender<Event>) {
    let key = task.key.clone();
    let status = |s: ExportStatus| {
        let _ = events.send(Event::Status(key.clone(), s));
    };
    let current = task.generation;
    let cancelled = || generation.load(Ordering::SeqCst) != current;
    if cancelled() {
        return status(ExportStatus::Cancelled);
    }
    let reencode = task.video.is_some() || task.profile != Profile::Original;
    // Normalizing needs loudness; otherwise it only adds ReplayGain tags when already known.
    if task.loudness.is_none() && reencode && task.normalize != Normalize::Off {
        status(ExportStatus::Measuring);
        match load_or_analyze(&task.source, &task.scan, &mut |_| {}) {
            Ok(map) => {
                let map = Arc::new(map);
                let _ = events.send(Event::Loudness(task.source.clone(), map.clone()));
                task.loudness = Some(map);
            }
            Err(e) => return status(ExportStatus::Failed(format!("measuring loudness: {e:#}"))),
        }
    }
    if let Some(map) = &task.loudness {
        apply_to_jobs(&mut task.jobs, map, task.normalize, reencode);
    }
    let total = task.jobs.len();
    let video = match task.video {
        Some(profile) => {
            status(ExportStatus::Installing(0.0));
            match tools::ensure_ffmpeg(&tools::dir(), &mut |p| status(ExportStatus::Installing(p)), &cancelled) {
                Ok(ffmpeg) => Some((ffmpeg, profile, Timeline::of(&task.source, task.scan.info.sample_rate))),
                Err(_) if cancelled() => return status(ExportStatus::Cancelled),
                Err(e) => return status(ExportStatus::Failed(format!("installing ffmpeg: {e}"))),
            }
        }
        None => None,
    };
    for (i, job) in task.jobs.iter().enumerate() {
        if cancelled() {
            return status(ExportStatus::Cancelled);
        }
        status(ExportStatus::Running { done: i, total, part: 0.0 });
        let result = match &video {
            Some((ffmpeg, profile, timeline)) => {
                let clip =
                    Clip { start: job.start, end: job.end, path: &job.path, tags: &job.tags, gain_db: job.gain_db };
                let mut last = 0.0;
                let r = export_clip(ffmpeg, &task.source, *timeline, &clip, *profile, &cancelled, &mut |p| {
                    if p - last >= 0.01 {
                        last = p;
                        status(ExportStatus::Running { done: i, total, part: p });
                    }
                });
                match r {
                    Err(_) if cancelled() => return status(ExportStatus::Cancelled),
                    r => r,
                }
            }
            None => export(&task.source, &task.scan, std::slice::from_ref(job), task.profile, &mut |_| {})
                .map_err(|e| format!("{e:#}")),
        };
        if let Err(e) = result {
            return status(ExportStatus::Failed(e));
        }
    }
    status(ExportStatus::Finished { dir: task.dir, count: total });
}

/// Where a recording's tracks go: a folder named after it, next to it.
pub fn export_dir_of(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    Some(path.parent()?.join(stem))
}

impl App {
    /// Queue a recording for export. Returns false if it can't be exported yet.
    fn enqueue(mut self, path: PathBuf) -> bool {
        let key = key_of(&path);
        if self.exports.peek().get(&key).is_some_and(|s| s.is_active()) {
            return true;
        }
        let scan = match self.scans.peek().get(&path) {
            Some(ScanState::Ready(s)) => s.clone(),
            _ => return false,
        };
        let Some(dir) = export_dir_of(&path) else { return false };
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let src_ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "mp3".into());
        let settings = self.cutlist.peek().export.clone();
        let profile = settings.profile.effective(&source_facts(&path, &scan));
        // A video is exported as video clips, never as audio.
        let video = splitter_core::is_video(&path).then_some(settings.video);
        let ext = match video {
            Some(v) => v.extension().to_string(),
            None => profile.extension(&src_ext).to_string(),
        };
        let planned = match self.cutlist.peek().recordings.get(&key) {
            Some(edit) => plan(edit, scan.info.total_samples, &stem, &settings),
            None => plan(&Default::default(), scan.info.total_samples, &stem, &settings),
        };
        if planned.is_empty() {
            self.exports.write().insert(key, ExportStatus::Failed("every track is left out".into()));
            return false;
        }
        let jobs = planned
            .iter()
            .map(|t| ExportJob {
                start: t.start,
                end: t.end,
                path: dir.join(format!("{}.{ext}", t.stem)),
                gain_db: 0.0,
                tags: Tags {
                    title: t.title.clone(),
                    album: stem.clone(),
                    track: t.number,
                    total: t.total,
                    replaygain: None,
                },
            })
            .collect();
        let exporter = self.exporter.peek().clone();
        let task = Task {
            key: key.clone(),
            generation: exporter.generation.load(Ordering::SeqCst),
            loudness: self.loudness.peek().get(&path).cloned(),
            source: path,
            scan,
            jobs,
            dir,
            profile,
            normalize: settings.normalize,
            video,
        };
        if exporter.tx.send(task).is_ok() {
            self.exports.write().insert(key, ExportStatus::Queued);
        }
        true
    }

    /// ⌘E: export the current recording.
    pub fn export_current(mut self) {
        let Some(path) = self.selected_path() else { return };
        if !self.enqueue(path) {
            self.error.set(Some("This recording can't be exported yet (still analysing, or nothing kept).".into()));
        }
    }

    /// Recordings marked done (and not exported since).
    pub fn done_recordings(&self) -> Vec<PathBuf> {
        let cutlist = self.cutlist.peek();
        self.recordings
            .peek()
            .iter()
            .filter(|r| cutlist.recordings.get(&key_of(&r.path)).is_some_and(|e| e.status == Status::Done))
            .map(|r| r.path.clone())
            .collect()
    }

    /// ⇧⌘E: export every recording marked done.
    pub fn export_done(mut self) {
        let paths = self.done_recordings();
        if paths.is_empty() {
            self.error.set(Some("No recording is marked done yet (⌘Enter marks one).".into()));
            return;
        }
        let skipped = paths.into_iter().filter(|p| !self.enqueue(p.clone())).count();
        if skipped > 0 {
            self.error.set(Some(format!("{skipped} recording(s) skipped: still analysing, or nothing kept.")));
        }
    }

    /// Stop after the current track and drop everything queued.
    pub fn cancel_exports(mut self) {
        self.exporter.peek().generation.fetch_add(1, Ordering::SeqCst);
        for status in self.exports.write().values_mut() {
            if status.is_active() {
                *status = ExportStatus::Cancelled;
            }
        }
    }

    /// Called from `tick`: follow the worker.
    pub fn poll_export(mut self) {
        let events = self.exporter.peek().events.clone();
        while let Ok(event) = events.try_recv() {
            match event {
                Event::Status(key, status) => {
                    // A cancel already marked it; ignore the worker's late progress.
                    let was_cancelled = self.exports.peek().get(&key) == Some(&ExportStatus::Cancelled);
                    if was_cancelled && status.is_active() {
                        continue;
                    }
                    if matches!(status, ExportStatus::Finished { .. }) {
                        if let Some(edit) = self.cutlist.write().recordings.get_mut(&key) {
                            edit.status = Status::Exported;
                        }
                        self.mark_dirty();
                        self.save_now();
                    }
                    self.exports.write().insert(key, status);
                }
                Event::Loudness(path, map) => {
                    self.loudness.write().insert(path, map);
                }
            }
        }
    }

    pub fn set_profile(mut self, profile: Profile) {
        if self.cutlist.peek().export.profile == profile {
            return;
        }
        self.ab_stop();
        self.cutlist.write().export.profile = profile;
        self.mark_dirty();
        self.save_now();
    }

    pub fn set_video_profile(mut self, profile: VideoProfile) {
        if self.cutlist.peek().export.video == profile {
            return;
        }
        self.cutlist.write().export.video = profile;
        self.mark_dirty();
        self.save_now();
    }

    pub fn set_normalize(mut self, normalize: Normalize) {
        self.cutlist.write().export.normalize = normalize;
        self.mark_dirty();
        self.save_now();
    }

    /// Open the export folder in Finder.
    pub fn reveal_export(&self, dir: &Path) {
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
}

/// What the size estimate and warnings need from a scan.
pub fn source_facts(path: &Path, scan: &Scan) -> SourceFacts {
    let info = &scan.info;
    let (kbps, bits) = match info.bitrate {
        Bitrate::Cbr(k) | Bitrate::Avg(k) => (k, None),
        Bitrate::Vbr { avg, .. } => (avg, None),
        Bitrate::Pcm { bits, kbps } => (kbps, Some(bits)),
        Bitrate::Unknown => ((info.file_size as f64 * 8.0 / 1000.0 / info.duration_secs().max(1.0)) as u32, None),
    };
    SourceFacts {
        lossy: bits.is_none(),
        copyable: can_copy(path, scan),
        kbps,
        sample_rate: info.sample_rate,
        channels: info.channels,
        bits,
    }
}
