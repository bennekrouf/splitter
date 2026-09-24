//! A/B listening: loop a short window and switch instantly between the original and how it
//! sounds with the chosen export profile.

use crate::state::App;
use dioxus::prelude::*;
use splitter_audio::source::open_source;
use splitter_audio::transcode::preview;
use splitter_audio::{AltPcm, Bitrate};
use splitter_core::export::Profile;
use std::sync::Arc;

/// Length of the compared window.
pub const AB_SECS: f64 = 12.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum AbState {
    #[default]
    Off,
    Preparing {
        profile: Profile,
        start: u64,
        end: u64,
    },
    Ready {
        profile: Profile,
        start: u64,
        end: u64,
        encoded: bool,
    },
}

pub type AbResult = Result<(Profile, u64, u64, Vec<f32>), String>;

impl App {
    /// A: start comparing at the playhead, or flip between original and encoded.
    pub fn ab_toggle(mut self) {
        let profile = self.cutlist.peek().export.profile;
        let pos = *self.pos.peek();
        let state = self.ab.peek().clone();
        match state {
            AbState::Ready { profile: p, start, end, encoded } if p == profile && pos >= start && pos <= end => {
                self.player().use_alt(!encoded);
                self.ab.set(AbState::Ready { profile, start, end, encoded: !encoded });
                return;
            }
            AbState::Preparing { .. } => return,
            _ => {}
        }
        if self.selected_path().is_some_and(|p| splitter_core::is_video(&p)) {
            self.error.set(Some("A/B compares audio formats; a video is exported as MP4 clips.".into()));
            return;
        }
        if profile == Profile::Original {
            self.error.set(Some(
                "Pick an export format first: Original is the source itself, so there is nothing to compare.".into(),
            ));
            return;
        }
        self.ab_start(profile, pos);
    }

    fn ab_start(mut self, profile: Profile, pos: u64) {
        let (Some(path), Some(scan)) = (self.selected_path(), self.current_scan()) else { return };
        self.ab_stop();
        let total = scan.info.total_samples;
        let len = (AB_SECS * scan.info.sample_rate as f64) as u64;
        let start = pos.min(total.saturating_sub(len));
        let end = (start + len).min(total);
        let bits = match scan.info.bitrate {
            Bitrate::Pcm { bits, .. } => Some(bits),
            _ => None,
        };
        let (tx, rx) = crossbeam_channel::bounded::<AbResult>(1);
        std::thread::Builder::new()
            .name("splitter-ab".into())
            .spawn(move || {
                let result = open_source(&path, scan.mp3.clone().map(Arc::new))
                    .and_then(|mut src| preview(profile, src.as_mut(), start, end, bits))
                    .map(|pcm| (profile, start, end, pcm))
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            })
            .expect("spawn A/B thread");
        self.ab_rx.set(Some(rx));
        self.ab.set(AbState::Preparing { profile, start, end });
    }

    /// Esc, file change or profile change: back to normal playback.
    pub fn ab_stop(mut self) {
        if *self.ab.peek() == AbState::Off {
            return;
        }
        let player = self.player();
        player.set_alt(None);
        player.set_loop(None);
        self.ab.set(AbState::Off);
        self.ab_rx.set(None);
    }

    /// Called from `tick`: pick up a finished preview and start looping it.
    pub fn poll_ab(mut self) {
        let Some(rx) = self.ab_rx.peek().clone() else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.ab_rx.set(None);
        match result {
            Ok((profile, start, end, pcm)) => {
                let player = self.player();
                player.set_alt(Some(AltPcm { pcm: Arc::new(pcm), start }));
                player.set_loop(Some((start, end)));
                player.seek(start);
                player.play();
                self.pos.set(start);
                self.reveal(start, true);
                self.ab.set(AbState::Ready { profile, start, end, encoded: false });
            }
            Err(e) => {
                self.error.set(Some(format!("A/B preview failed: {e}")));
                self.ab.set(AbState::Off);
            }
        }
    }
}
