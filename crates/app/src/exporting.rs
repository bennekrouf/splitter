//! Exporting the current recording's tracks on a background thread.

use crate::state::{key_of, App, ExportMsg, ExportState};
use dioxus::prelude::*;
use splitter_audio::export::{export, ExportJob, Tags};
use splitter_core::export::plan;
use splitter_core::Status;
use std::path::PathBuf;

impl App {
    /// Where the current recording's tracks go: a folder named after it, next to it.
    pub fn export_dir(&self) -> Option<PathBuf> {
        let path = self.selected_path()?;
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        Some(path.parent()?.join(stem))
    }

    /// ⌘E: write every kept track of the current recording.
    pub fn export_current(mut self) {
        if matches!(*self.export.peek(), ExportState::Running { .. }) {
            return;
        }
        let (Some(path), Some(scan), Some(dir)) = (self.selected_path(), self.current_scan(), self.export_dir()) else {
            return;
        };
        let key = key_of(&path);
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "mp3".into());
        let settings = self.cutlist.peek().export.clone();
        let Some(planned) = self.peek_edit(|e| plan(e, scan.info.total_samples, &stem, &settings)) else { return };
        if planned.is_empty() {
            self.error.set(Some("Every track is dropped; there is nothing to export.".into()));
            return;
        }
        let jobs: Vec<ExportJob> = planned
            .iter()
            .map(|t| ExportJob {
                start: t.start,
                end: t.end,
                path: dir.join(format!("{}.{ext}", t.stem)),
                tags: Tags { title: t.title.clone(), album: stem.clone(), track: t.number, total: t.total },
            })
            .collect();

        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("splitter-export".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let result = export(&path, &scan, &jobs, &mut |n| {
                    let _ = progress_tx.send(ExportMsg::Progress(n));
                });
                let _ = tx.send(match result {
                    Ok(()) => ExportMsg::Finished,
                    Err(e) => ExportMsg::Failed(format!("{e:#}")),
                });
            })
            .expect("spawn export thread");
        self.export_rx.set(Some(rx));
        self.export.set(ExportState::Running { key, done: 0, total: planned.len() });
    }

    /// Called from `tick`: follow a running export.
    pub fn poll_export(mut self) {
        let Some(rx) = self.export_rx.peek().clone() else { return };
        while let Ok(msg) = rx.try_recv() {
            let ExportState::Running { key, done, total } = self.export.peek().clone() else { return };
            match msg {
                ExportMsg::Progress(n) => {
                    if n != done {
                        self.export.set(ExportState::Running { key, done: n, total });
                    }
                }
                ExportMsg::Finished => {
                    let dir = self.export_dir_for(&key).unwrap_or_default();
                    if let Some(edit) = self.cutlist.write().recordings.get_mut(&key) {
                        edit.status = Status::Exported;
                    }
                    self.mark_dirty();
                    self.save_now();
                    self.export.set(ExportState::Finished { key, dir, count: total });
                    self.export_rx.set(None);
                }
                ExportMsg::Failed(e) => {
                    self.error.set(Some(format!("Export failed: {e}")));
                    self.export.set(ExportState::Idle);
                    self.export_rx.set(None);
                }
            }
        }
    }

    fn export_dir_for(&self, key: &str) -> Option<PathBuf> {
        let folder = self.folder.peek().clone()?;
        let stem = std::path::Path::new(key).file_stem()?.to_string_lossy().into_owned();
        Some(folder.join(stem))
    }

    /// Open the export folder in Finder.
    pub fn reveal_export(&self, dir: &std::path::Path) {
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
}
