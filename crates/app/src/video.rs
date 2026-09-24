//! Video preview: a video file's picture in place of the overview waveform, following the
//! audio player.
//!
//! The web view plays the file muted from the local media server (or through the app's own
//! protocol, if it won't load from 127.0.0.1). The audio stays on the
//! player (sample-accurate, with A/B, loops and preview cuts), and the picture is steered to
//! its playhead: the exact frame while paused; while playing, a jump when the playhead jumps or
//! the picture drifts off.

use crate::media_server;
use crate::state::App;
use crate::video_export::Timeline;
use dioxus::desktop::use_asset_handler;
use dioxus::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

/// Installs `window.splitterVideo.sync(t, playing, hard)`, which steers the `<video>`.
const SYNC_JS: &str = r#"
window.splitterVideo = {
  // How far ahead of the playhead to aim a jump while playing: the web view takes a moment to
  // seek and to start playing, and then plays smoothly. Learned from how far off it settles,
  // and kept from one video to the next (about 0.4 s in WebKit on macOS).
  lead: (window.splitterVideo && window.splitterVideo.lead) || 0.4,
  jumped: false,
  // The previous drift reading while playing: a jump stalls the picture for a moment, so only
  // two readings that agree are worth acting on.
  prev: null,
  jump(v, t) {
    v.currentTime = t + this.lead;
    this.jumped = true;
    this.prev = null;
  },
  sync(t, playing, hard) {
    const v = document.getElementById('video-preview');
    if (!v || v.readyState < 1) return;
    v.muted = true;
    if (!playing) {
      if (!v.paused) v.pause();
      this.jumped = false;
      this.prev = null;
      if (Math.abs(v.currentTime - t) > 0.001) v.currentTime = t;
      return;
    }
    if (v.paused) v.play().catch(() => {});
    if (hard) return this.jump(v, t);
    // Another seek now would restart this one and it would never land.
    if (v.seeking) return;
    const d = v.currentTime - t;
    const settled = this.prev !== null && Math.abs(d - this.prev) < 0.03;
    this.prev = d;
    if (!settled) return;
    if (this.jumped) {
      this.lead = Math.max(0, Math.min(1.5, this.lead - d));
      this.jumped = false;
    }
    // Speed changes aren't honoured by every web view: correct drift with a jump instead.
    if (Math.abs(d) > 0.06) this.jump(v, t);
  }
};
"#;

/// What was last sent to the `<video>`, to decide when it needs steering again.
struct Sent {
    secs: f64,
    playing: bool,
    at: Instant,
    /// `loads` when last sent: after (re)loading, the position is sent whatever it is.
    loads: u32,
}

#[component]
pub fn VideoPreview(path: PathBuf, rate: u32) -> Element {
    let app = use_context::<App>();
    let url = use_hook(|| media_server::publish(&path));
    // Our playhead counts the AAC priming the video's own timeline skips.
    let timeline = use_hook(|| Timeline::of(&path, rate));
    let served = url.clone();
    use_drop(move || {
        if let Some(url) = &served {
            media_server::unpublish(url);
        }
    });
    let mut failed = use_signal(|| false);
    // Loading through the app's own protocol, after the web view refused 127.0.0.1.
    let mut in_app = use_signal(|| false);
    use_asset_handler("media", move |request, responder| {
        std::thread::spawn(move || responder.respond(media_server::in_app_response(&request)));
    });
    // Bumped when the video's metadata loads: only then can it be positioned.
    let mut loads = use_signal(|| 0u32);
    let sent = use_hook(|| Rc::new(RefCell::new(Sent { secs: -1.0, playing: false, at: Instant::now(), loads: 0 })));

    use_hook(|| {
        document::eval(SYNC_JS);
    });

    use_effect(move || {
        let secs = timeline.secs((app.pos)());
        let playing = (app.playing)();
        let loads = loads();
        let mut s = sent.borrow_mut();
        let elapsed = s.at.elapsed().as_secs_f64();
        let hard = loads != s.loads || playing != s.playing || {
            // Where the video should be by now if nothing jumped since the last sync.
            let expected = if s.playing { s.secs + elapsed } else { s.secs };
            (secs - expected).abs() > 0.15
        };
        let due = if playing { hard || elapsed > 0.25 } else { hard || secs != s.secs };
        if !due {
            return;
        }
        *s = Sent { secs, playing, at: Instant::now(), loads };
        document::eval(&format!("window.splitterVideo && window.splitterVideo.sync({secs}, {playing}, {hard})"));
    });

    let Some(url) = url else {
        return rsx! {
            div { class: "video-preview",
                div { class: "video-failed", "The video can't be shown (no local server). Its audio plays and splits as usual." }
            }
        };
    };
    let src = if in_app() { media_server::in_app_path(&url) } else { url };
    rsx! {
        div { class: "video-preview",
            video {
                id: "video-preview",
                src: "{src}",
                muted: true,
                playsinline: true,
                preload: "auto",
                onloadedmetadata: move |_| {
                    failed.set(false);
                    loads += 1;
                },
                onerror: move |_| {
                    // Once through the app's own protocol before giving up: the web view may
                    // refuse local addresses. A format it can't play fails there too.
                    if in_app() {
                        failed.set(true);
                    } else {
                        in_app.set(true);
                    }
                },
                onclick: move |_| {
                    app.toggle();
                    app.focus_root();
                },
            }
            if failed() {
                div { class: "video-failed",
                    "This video can't be shown here (its format isn't supported by the system's web view). "
                    "Its audio plays and splits as usual."
                }
            }
        }
    }
}
