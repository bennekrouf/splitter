//! Background worker that scans recordings one at a time, current selection first.

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
}

#[derive(Clone)]
pub struct Scanner {
    queue: Arc<(Mutex<VecDeque<PathBuf>>, Condvar)>,
    events: Receiver<ScanEvent>,
}

impl Scanner {
    pub fn spawn() -> Self {
        let queue = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let (tx, events) = crossbeam_channel::unbounded();
        let q = queue.clone();
        std::thread::Builder::new()
            .name("splitter-scan".into())
            .spawn(move || worker(q, tx))
            .expect("spawn scan thread");
        Self { queue, events }
    }

    /// Replace the pending queue (e.g. a new folder was opened).
    pub fn set_queue(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.clear();
        q.extend(paths);
        cv.notify_one();
    }

    /// Move `path` to the front of the queue (adding it if missing).
    pub fn prioritize(&self, path: PathBuf) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.retain(|p| p != &path);
        q.push_front(path);
        cv.notify_one();
    }

    pub fn try_recv(&self) -> Option<ScanEvent> {
        self.events.try_recv().ok()
    }
}

fn worker(queue: Arc<(Mutex<VecDeque<PathBuf>>, Condvar)>, tx: Sender<ScanEvent>) {
    let (lock, cv) = &*queue;
    loop {
        let path = {
            let mut q = lock.lock().unwrap();
            loop {
                if let Some(p) = q.pop_front() {
                    break p;
                }
                q = cv.wait(q).unwrap();
            }
        };
        let _ = tx.send(ScanEvent::Started(path.clone()));
        let mut progress = |p: f32| {
            let _ = tx.send(ScanEvent::Progress(path.clone(), p));
        };
        let event = match load_or_scan(&path, &mut progress) {
            Ok(scan) => ScanEvent::Done(path, Arc::new(scan)),
            Err(e) => ScanEvent::Failed(path, format!("{e:#}")),
        };
        if tx.send(event).is_err() {
            return;
        }
    }
}
