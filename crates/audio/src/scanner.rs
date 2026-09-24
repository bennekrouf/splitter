//! Background worker that scans recordings one at a time, current selection first. Once no
//! scan is waiting, it measures loudness for the scanned files (slower, and not needed to start
//! working on a file).

use crate::loudness::{load_or_analyze, LoudnessMap};
use crate::scan::{load_or_scan, Scan};
use crossbeam_channel::{Receiver, Sender};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

pub enum ScanEvent {
    Started(PathBuf),
    Progress(PathBuf, f32),
    Done(PathBuf, Arc<Scan>),
    Failed(PathBuf, String),
    Loudness(PathBuf, Arc<LoudnessMap>),
}

#[derive(Default)]
struct Queues {
    scans: VecDeque<PathBuf>,
    loudness: VecDeque<(PathBuf, Arc<Scan>)>,
}

#[derive(Clone)]
pub struct Scanner {
    queue: Arc<(Mutex<Queues>, Condvar)>,
    events: Receiver<ScanEvent>,
}

impl Scanner {
    pub fn spawn() -> Self {
        let queue = Arc::new((Mutex::new(Queues::default()), Condvar::new()));
        let (tx, events) = crossbeam_channel::unbounded();
        let q = queue.clone();
        std::thread::Builder::new()
            .name("splitter-scan".into())
            .spawn(move || worker(q, tx))
            .expect("spawn scan thread");
        Self { queue, events }
    }

    /// Replace the pending work (e.g. a new folder was opened).
    pub fn set_queue(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.scans.clear();
        q.scans.extend(paths);
        q.loudness.clear();
        cv.notify_one();
    }

    /// Move `path` to the front of the scan queue (adding it if missing).
    pub fn prioritize(&self, path: PathBuf) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.scans.retain(|p| p != &path);
        q.scans.push_front(path);
        cv.notify_one();
    }

    /// Measure loudness for an already-scanned file, ahead of the others.
    pub fn prioritize_loudness(&self, path: PathBuf, scan: Arc<Scan>) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.loudness.retain(|(p, _)| p != &path);
        q.loudness.push_front((path, scan));
        cv.notify_one();
    }

    pub fn try_recv(&self) -> Option<ScanEvent> {
        self.events.try_recv().ok()
    }
}

enum Work {
    Scan(PathBuf),
    Loudness(PathBuf, Arc<Scan>),
}

fn worker(queue: Arc<(Mutex<Queues>, Condvar)>, tx: Sender<ScanEvent>) {
    let (lock, cv) = &*queue;
    loop {
        let work = {
            let mut q = lock.lock().unwrap();
            loop {
                if let Some(p) = q.scans.pop_front() {
                    break Work::Scan(p);
                }
                if let Some((p, s)) = q.loudness.pop_front() {
                    break Work::Loudness(p, s);
                }
                q = cv.wait(q).unwrap();
            }
        };
        let event = match work {
            Work::Scan(path) => {
                let _ = tx.send(ScanEvent::Started(path.clone()));
                let mut progress = |p: f32| {
                    let _ = tx.send(ScanEvent::Progress(path.clone(), p));
                };
                match load_or_scan(&path, &mut progress) {
                    Ok(scan) => {
                        let scan = Arc::new(scan);
                        lock.lock().unwrap().loudness.push_back((path.clone(), scan.clone()));
                        ScanEvent::Done(path, scan)
                    }
                    Err(e) => ScanEvent::Failed(path, format!("{e:#}")),
                }
            }
            Work::Loudness(path, scan) => match load_or_analyze(&path, &scan, &mut |_| {}) {
                Ok(map) => ScanEvent::Loudness(path, Arc::new(map)),
                Err(e) => {
                    // Loudness is informational; the file stays fully usable without it.
                    eprintln!("splitter: loudness analysis failed for {}: {e:#}", path.display());
                    continue;
                }
            },
        };
        if tx.send(event).is_err() {
            return;
        }
    }
}
