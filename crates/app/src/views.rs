use crate::ab::AbState;
use crate::download::Phase;
use crate::exporting::source_facts;
use crate::exporting::ExportStatus;
use crate::review;
use crate::state::{self, key_of, ScanState};
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;
use splitter_audio::{Bitrate, Scan};
use splitter_core::edit::SplitState;
use splitter_core::export::{plan, Normalize, Profile};
use splitter_core::loudness::Loudness;
use splitter_core::time::{fmt_precise, fmt_short};
use splitter_core::tracklist;
use splitter_core::Status;
use std::fmt::Write;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

const CSS: &str = include_str!("../assets/style.css");
/// Waveform resolution; the SVG is stretched to the element width.
const COLS: usize = 1600;
/// How close (in pixels) a click must be to grab a split marker.
const GRAB_PX: f64 = 6.0;

#[component]
pub fn App() -> Element {
    let app = use_context_provider(state::App::new);

    use_hook(move || match std::env::args().nth(1) {
        Some(arg) => app.open(std::path::Path::new(&arg)),
        None => {
            if let Some(dir) = state::prefs::last_folder() {
                app.open(&dir);
            }
        }
    });

    use_future(move || async move {
        loop {
            tokio::time::sleep(Duration::from_millis(16)).await;
            app.tick();
        }
    });

    // Re-send the preview's skipped ranges only when they actually change (a marker drag
    // writes the cutlist on every mouse move).
    let skips = use_memo(move || app.wanted_skips());
    use_effect(move || app.player().set_skips(skips()));

    // Delayed and best-effort: a release check is never worth slowing a cold start, and a
    // failed one is not worth mentioning.
    let mut update = use_signal(|| Option::<crate::update_check::UpdateInfo>::None);
    use_future(move || async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        if let Some(info) = crate::update_check::check().await {
            update.set(Some(info));
        }
    });

    let sidebar_width = (app.sidebar_width)();
    let sidebar_drag = app.sidebar_drag;

    let onkeydown = move |e: KeyboardEvent| {
        if app.paste.peek().is_some()
            || app.editing_title.peek().is_some()
            || app.url_dialog.peek().is_some()
            || *app.renaming.peek()
            || app.confirm_delete.peek().is_some()
        {
            return; // a text field has the keyboard
        }
        let m = e.modifiers();
        let cmd = m.meta() || m.ctrl();
        let step = if m.alt() {
            0.01
        } else if m.shift() {
            0.5
        } else {
            5.0
        };
        let nudge = if m.shift() { 0.1 } else { 0.01 };
        let handled = match e.code() {
            // Playback and navigation
            Code::Space => {
                app.toggle();
                true
            }
            Code::ArrowLeft => {
                app.nudge(-step);
                true
            }
            Code::ArrowRight => {
                app.nudge(step);
                true
            }
            Code::ArrowUp => {
                app.select_offset(-1);
                true
            }
            Code::ArrowDown => {
                app.select_offset(1);
                true
            }
            Code::Home => {
                app.seek(0, true);
                true
            }
            Code::End => {
                app.seek(u64::MAX, true);
                true
            }
            Code::Equal | Code::NumpadAdd => {
                app.zoom(0.5);
                true
            }
            Code::Minus | Code::NumpadSubtract => {
                app.zoom(2.0);
                true
            }
            Code::KeyO if cmd => {
                open_folder(app);
                true
            }
            Code::KeyU if cmd => {
                app.ask_url();
                true
            }
            // Split review
            Code::Tab => {
                app.step_split(!m.shift());
                true
            }
            Code::Enter | Code::NumpadEnter if cmd => {
                app.finish_recording();
                true
            }
            Code::Enter | Code::NumpadEnter => {
                app.confirm_and_next();
                true
            }
            Code::Backspace | Code::Delete => {
                app.delete_split();
                true
            }
            Code::Comma => {
                app.nudge_split(-nudge);
                true
            }
            Code::Period => {
                app.nudge_split(nudge);
                true
            }
            Code::KeyS if !cmd => {
                app.snap_split();
                true
            }
            Code::KeyM if !cmd => {
                app.add_split_at_playhead();
                true
            }
            Code::KeyC if !cmd => {
                app.replay_split();
                true
            }
            Code::KeyZ if cmd && m.shift() => {
                app.redo();
                true
            }
            Code::KeyZ if cmd => {
                app.undo();
                true
            }
            // Tracks
            Code::KeyT if !cmd => {
                app.edit_title(None);
                true
            }
            Code::KeyX if !cmd => {
                app.toggle_drop(None);
                true
            }
            Code::KeyP if !cmd => {
                app.toggle_preview();
                true
            }
            Code::KeyR if !cmd => {
                let mut renaming = app.renaming;
                renaming.set(true);
                true
            }
            Code::KeyG if !cmd => {
                app.toggle_silence();
                true
            }
            Code::KeyE if cmd && m.shift() => {
                app.export_done();
                true
            }
            Code::KeyE if cmd => {
                app.export_current();
                true
            }
            Code::KeyF if !cmd => {
                app.toggle_flag();
                true
            }
            Code::KeyA if !cmd => {
                app.ab_toggle();
                true
            }
            Code::Escape => {
                if *app.ab.peek() != AbState::Off {
                    app.ab_stop();
                } else {
                    let (mut cur, mut closed) = (app.cur_split, app.closed_track);
                    cur.set(None);
                    closed.set(None);
                }
                true
            }
            _ => false,
        };
        if handled {
            e.prevent_default();
        }
    };

    rsx! {
        style { {CSS} }
        div {
            class: if sidebar_drag() { "app resizing" } else { "app" },
            style: "grid-template-columns: {sidebar_width}px 1fr",
            tabindex: "0",
            onkeydown,
            // Dragging the file list's edge: follow the mouse anywhere in the window.
            onmousemove: move |e| {
                if sidebar_drag() {
                    let held = e.held_buttons().contains(MouseButton::Primary);
                    app.set_sidebar_width(e.client_coordinates().x, !held);
                }
            },
            onmouseup: move |e| {
                if sidebar_drag() {
                    app.set_sidebar_width(e.client_coordinates().x, true);
                }
            },
            onmounted: move |e| {
                let mut root = app.root;
                root.set(Some(e.data()));
                app.focus_root();
            },
            Sidebar {}
            main { class: "main", Editor {} }
            UrlDialog {}
            DeleteDialog {}
        }
        // Dismissed for this session only: the next launch asks again.
        if let Some(info) = update() {
            div { class: "update-banner",
                span { class: "update-banner-text",
                    "Splitter "
                    strong { "{info.latest_version}" }
                    " is available (you have {env!(\"CARGO_PKG_VERSION\")})."
                }
                a { class: "update-banner-link", href: "{info.download_url}", target: "_blank", "Download" }
                button { class: "update-banner-dismiss", title: "Dismiss", onclick: move |_| update.set(None), "×" }
            }
        }
    }
}

fn open_folder(app: state::App) {
    spawn(async move {
        if let Some(dir) = rfd::AsyncFileDialog::new().set_title("Open a folder of recordings").pick_folder().await {
            app.open(dir.path());
        }
        app.focus_root();
    });
}

#[component]
fn Sidebar() -> Element {
    let app = use_context::<state::App>();
    let recordings = app.recordings.read();
    let selected = (app.selected)();
    let scans = app.scans.read();
    let cutlist = app.cutlist.read();
    let exports = app.exports.read();
    let folder = app.folder.read().as_ref().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned());
    let active = exports.values().filter(|s| s.is_active()).count();
    let done = recordings
        .iter()
        .filter(|r| cutlist.recordings.get(&key_of(&r.path)).is_some_and(|e| e.status == Status::Done))
        .count();

    // Keep the selected file visible, and show it in the window title.
    use_effect(move || {
        let i = (app.selected)();
        let name = i.and_then(|i| app.recordings.peek().get(i).map(|r| r.name()));
        dioxus::desktop::window().set_title(&match name {
            Some(n) => format!("{n} — Splitter"),
            None => "Splitter".into(),
        });
        document::eval(
            "requestAnimationFrame(() => document.querySelector('.item.selected')?.scrollIntoView({block: 'nearest'}))",
        );
    });

    rsx! {
        aside { class: "sidebar",
            div {
                class: "sidebar-edge",
                title: "Drag to resize · double-click to fit the longest name",
                onmousedown: move |e| {
                    e.prevent_default();
                    let mut drag = app.sidebar_drag;
                    drag.set(true);
                },
                ondoubleclick: move |_| app.fit_sidebar(),
            }
            div { class: "sidebar-head",
                span { class: "brand", "Splitter" }
                div { class: "head-actions",
                    button { title: "Open a folder of recordings (⌘O)", onclick: move |_| open_folder(app), "Open folder…" }
                    button {
                        title: "Download a YouTube video's audio and open it (⌘U)",
                        disabled: app.download.read().is_some(),
                        onclick: move |_| app.ask_url(),
                        "URL…"
                    }
                }
            }
            DownloadStatus {}
            if let Some(name) = folder {
                div { class: "folder", title: "{name}", "{name}" }
            }
            div { class: "queue",
                for (i, rec) in recordings.iter().enumerate() {
                    {
                        let edit = cutlist.recordings.get(&key_of(&rec.path));
                        let status = edit.map(|e| e.status).unwrap_or_default();
                        let (dot, dot_title) = match status {
                            Status::Done | Status::Exported => ("dot done", "done"),
                            Status::InProgress => ("dot active", "in progress"),
                            Status::Flagged => ("dot flagged", "flagged"),
                            Status::Todo => ("dot", "to do"),
                        };
                        let counts = edit.filter(|e| !e.splits.is_empty()).map(|e| e.counts());
                        let export = exports.get(&key_of(&rec.path)).cloned();
                        rsx! {
                            div {
                                key: "{rec.path.display()}",
                                class: if selected == Some(i) { "item selected" } else { "item" },
                                onclick: move |_| app.select(i),
                                span { class: "{dot}", title: "{dot_title}" }
                                span { class: "item-name", title: "{rec.name()}", "{rec.name()}" }
                                {
                                    let path = rec.path.clone();
                                    rsx! {
                                        button {
                                            class: "item-trash",
                                            title: "Move to the Trash…",
                                            onclick: move |e| {
                                                e.stop_propagation();
                                                let mut confirm = app.confirm_delete;
                                                confirm.set(Some(path.clone()));
                                            },
                                            "🗑"
                                        }
                                    }
                                }
                                match (export, scans.get(&rec.path)) {
                                    (Some(ExportStatus::Queued), _) => rsx! { span { class: "item-meta busy", "queued" } },
                                    (Some(ExportStatus::Measuring), _) => rsx! { span { class: "item-meta busy", "measuring…" } },
                                    (Some(ExportStatus::Running { done, total }), _) => rsx! {
                                        span { class: "item-meta busy", "exporting {done}/{total}" }
                                    },
                                    (Some(ExportStatus::Failed(e)), _) => rsx! { span { class: "item-meta bad", title: "{e}", "export failed" } },
                                    (_, Some(ScanState::Scanning(p))) => rsx! { span { class: "item-meta busy", "{(p * 100.0) as u32}%" } },
                                    (_, Some(ScanState::Failed(e))) => rsx! { span { class: "item-meta bad", title: "{e}", "error" } },
                                    (_, Some(ScanState::Ready(s))) => rsx! {
                                        if let Some((ok, todo)) = counts {
                                            span {
                                                class: if todo == 0 { "item-meta ok" } else { "item-meta todo" },
                                                title: "{ok + todo + 1} tracks: {ok} splits confirmed, {todo} to review",
                                                if todo == 0 { "{ok + 1} tracks ✓" } else { "{todo} to review" }
                                            }
                                        } else {
                                            span { class: "item-meta", "{fmt_short(s.info.duration_secs())}" }
                                        }
                                    },
                                    (_, None) => rsx! { span { class: "item-meta", "…" } },
                                }
                            }
                        }
                    }
                }
            }
            if recordings.is_empty() {
                div { class: "hint", "Open a folder containing MP3 or WAV recordings, or MP4/MOV videos (⌘O), or a YouTube link (⌘U)." }
            }
            div { class: "sidebar-foot",
                if active > 0 {
                    span { class: "dim", "Exporting {active} file(s)…" }
                    button { onclick: move |_| app.cancel_exports(), "Cancel" }
                } else {
                    button {
                        class: "primary",
                        disabled: done == 0,
                        title: "Export every file marked done (⌘Enter marks one)",
                        onclick: move |_| app.export_done(),
                        "Export {done} done file(s)"
                        kbd { "⇧⌘E" }
                    }
                }
            }
        }
    }
}

/// Identity-compared handle so waveform components only re-render when the scan changes.
#[derive(Clone)]
struct ScanRef(Arc<Scan>);

impl PartialEq for ScanRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[component]
fn Editor() -> Element {
    let app = use_context::<state::App>();
    let recordings = app.recordings.read();
    let Some(i) = (app.selected)() else {
        return rsx! {
            div { class: "empty", "No recording selected." }
            ErrorBanner {}
        };
    };
    let Some(rec) = recordings.get(i) else { return rsx! {} };
    let scans = app.scans.read();
    let body = match scans.get(&rec.path) {
        Some(ScanState::Ready(scan)) => {
            let scan = ScanRef(scan.clone());
            rsx! {
                Info { scan: scan.clone() }
                Overview { scan: scan.clone() }
                Detail { scan: scan.clone() }
                Transport { scan: scan.clone() }
                ReviewBar { scan: scan.clone() }
                ExportBar { scan: scan.clone() }
                TrackList { scan }
                PasteDialog {}
            }
        }
        Some(ScanState::Failed(e)) => rsx! { div { class: "empty bad", "Could not read this file: {e}" } },
        Some(ScanState::Scanning(p)) => rsx! {
            div { class: "empty",
                "Analysing… {(p * 100.0) as u32}%"
                div { class: "progress", div { style: "width: {p * 100.0}%" } }
            }
        },
        None => rsx! { div { class: "empty", "Waiting to analyse…" } },
    };

    rsx! {
        FileName { key: "{rec.name()}", path: rec.path.clone() }
        {body}
        ErrorBanner {}
        Keys {}
    }
}

/// The recording's name. ✎ (or R) renames the file, and with it the export folder.
#[component]
fn FileName(path: std::path::PathBuf) -> Element {
    let app = use_context::<state::App>();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut draft = use_signal(|| stem.clone());
    let mut renaming = app.renaming;
    let original = use_signal(|| stem.clone());
    let mut close = move |save: bool| {
        if !*renaming.peek() {
            return;
        }
        renaming.set(false);
        let name = draft.peek().clone();
        // Start from the current name next time (after a rename this component is remounted).
        draft.set(original.peek().clone());
        if save && !name.trim().is_empty() {
            app.rename_recording(&name);
        }
        app.focus_root();
    };
    if !renaming() {
        return rsx! {
            div { class: "file-name",
                h1 { class: "title", "{stem}" span { class: "dim", "{ext}" } }
                button {
                    class: "rename",
                    title: "Rename the file and its export folder (R)",
                    onclick: move |_| renaming.set(true),
                    "✎"
                }
                span { class: "spacer" }
                ApplyCuts {}
            }
        };
    }
    rsx! {
        div { class: "file-name",
            input {
                class: "title-input name-input",
                value: "{draft}",
                onmounted: move |e| async move {
                    let _ = e.set_focus(true).await;
                },
                oninput: move |e| draft.set(e.value()),
                onkeydown: move |e| {
                    e.stop_propagation();
                    match e.key() {
                        Key::Enter => close(true),
                        Key::Escape => close(false),
                        _ => {}
                    }
                },
                onblur: move |_| close(true),
            }
            span { class: "dim", "{ext}" }
            span { class: "dim hint", "Enter renames the file and its export folder · Esc cancels" }
        }
    }
}

/// Write a copy without the cut parts; it opens as a new entry and the original stays.
#[component]
fn ApplyCuts() -> Element {
    let app = use_context::<state::App>();
    // Subscribe to what `can_apply_cuts` peeks at.
    let _ = (app.cutlist.read(), app.scans.read(), app.selected.read());
    if let Some(a) = app.applying.read().as_ref() {
        let pct = (a.progress * 100.0) as u32;
        return rsx! {
            span { class: "applying dim", "Applying cuts… {pct}%" }
            div { class: "progress small", div { style: "width: {pct}%" } }
        };
    }
    let can = app.can_apply_cuts();
    rsx! {
        button {
            class: "primary",
            disabled: !can,
            title: if can {
                "Save a copy without the cut silence and left-out tracks, and open it. The original stays; ⌘Z right after removes the copy."
            } else {
                "Nothing is cut in this recording"
            },
            onclick: move |_| app.apply_cuts(),
            "✂ Apply cuts"
        }
    }
}

/// "Move to the Trash?" for a recording in the file list. Enter confirms, Esc cancels.
#[component]
fn DeleteDialog() -> Element {
    let app = use_context::<state::App>();
    let Some(path) = (app.confirm_delete)() else { return rsx! {} };
    let name = key_of(&path);
    let edits = app.cutlist.read().recordings.get(&name).is_some_and(|e| !e.splits.is_empty());
    let close = move || {
        let mut confirm = app.confirm_delete;
        confirm.set(None);
        app.focus_root();
    };
    let confirm = move || {
        let target = app.confirm_delete.peek().clone();
        close();
        if let Some(p) = target {
            app.delete_recording(&p);
        }
    };
    rsx! {
        div { class: "modal-backdrop", onclick: move |_| close(),
            div {
                class: "modal",
                tabindex: "0",
                onclick: move |e| e.stop_propagation(),
                onmounted: move |e| async move {
                    let _ = e.set_focus(true).await;
                },
                onkeydown: move |e| {
                    e.stop_propagation();
                    match e.key() {
                        Key::Enter => confirm(),
                        Key::Escape => close(),
                        _ => {}
                    }
                },
                h2 { "Move to the Trash?" }
                p { class: "delete-name", "{name}" }
                p { class: "dim",
                    "The file goes to the system Trash, so you can still restore it from there."
                    if edits { " Its splits and titles in Splitter are removed." }
                    " Tracks you already exported are not touched."
                }
                div { class: "modal-actions",
                    button { onclick: move |_| close(), "Cancel" kbd { "Esc" } }
                    button { class: "danger", onclick: move |_| confirm(), "Move to Trash" kbd { "Enter" } }
                }
            }
        }
    }
}

#[component]
fn ErrorBanner() -> Element {
    let app = use_context::<state::App>();
    let Some(err) = app.error.read().clone() else { return rsx! {} };
    rsx! {
        div {
            class: "error",
            onclick: move |_| {
                let mut e = app.error;
                e.set(None);
            },
            "{err}"
        }
    }
}

/// Progress of the URL download, under the sidebar header.
#[component]
fn DownloadStatus() -> Element {
    let app = use_context::<state::App>();
    let Some(phase) = app.download.read().as_ref().map(|d| d.phase) else { return rsx! {} };
    let (label, frac) = match phase {
        Phase::Installing(p) => (format!("Setting up the downloader (first time)… {}%", (p * 100.0) as u32), Some(p)),
        Phase::Starting => ("Fetching video info…".to_string(), None),
        Phase::Downloading(Some(p)) => (format!("Downloading… {}%", (p * 100.0) as u32), Some(p)),
        Phase::Downloading(None) => ("Downloading…".to_string(), None),
        Phase::Converting(Some(p)) => (format!("Converting to WAV… {}%", (p * 100.0) as u32), Some(p)),
        Phase::Converting(None) => ("Converting to WAV…".to_string(), None),
    };
    rsx! {
        div { class: "download",
            div { class: "download-row",
                span { class: "dim", "{label}" }
                button { onclick: move |_| app.cancel_download(), "Cancel" }
            }
            div { class: if frac.is_none() { "progress indeterminate" } else { "progress" },
                div { style: "width: {frac.unwrap_or(1.0) * 100.0}%" }
            }
        }
    }
}

/// ⌘U: paste a YouTube link to download its audio.
#[component]
fn UrlDialog() -> Element {
    let app = use_context::<state::App>();
    let Some(text) = (app.url_dialog)() else { return rsx! {} };
    let mut dialog = app.url_dialog;
    let close = move || {
        let mut dialog = app.url_dialog;
        dialog.set(None);
        app.focus_root();
    };
    let go = move || {
        // Take the text first: a guard held by the `if let` would still be alive when `close`
        // writes the signal, which panics.
        let url = app.url_dialog.peek().clone();
        if let Some(url) = url {
            close();
            app.start_download(&url);
        }
    };
    let dir = crate::download::downloads_dir();

    rsx! {
        div { class: "modal-backdrop", onclick: move |_| close(),
            div { class: "modal", onclick: move |e| e.stop_propagation(),
                h2 { "Open from URL" }
                p { class: "dim",
                    "The audio is downloaded, converted to WAV and saved in {dir.display()}. "
                    "Works with YouTube and most other video sites. "
                    "The first time, the downloader (yt-dlp, about 75 MB) is set up automatically."
                }
                input {
                    class: "url",
                    // Uncontrolled: binding `value` makes fast typing drop characters on re-render.
                    r#type: "url",
                    placeholder: "https://www.youtube.com/watch?v=…",
                    spellcheck: "false",
                    onmounted: move |e| async move {
                        let _ = e.set_focus(true).await;
                    },
                    oninput: move |e| dialog.set(Some(e.value())),
                    onkeydown: move |e| {
                        e.stop_propagation();
                        match e.key() {
                            Key::Escape => close(),
                            Key::Enter => go(),
                            _ => {}
                        }
                    },
                }
                div { class: "modal-actions",
                    button { onclick: move |_| close(), "Cancel" }
                    button { class: "primary", disabled: text.trim().is_empty(), onclick: move |_| go(),
                        "Download"
                        kbd { "Enter" }
                    }
                }
            }
        }
    }
}

#[component]
fn Info(scan: ScanRef) -> Element {
    let i = &scan.0.info;
    let bitrate = match &i.bitrate {
        Bitrate::Cbr(k) => format!("CBR {k} kbps"),
        Bitrate::Vbr { min, avg, max } => format!("VBR {min}–{max} kbps (avg {avg})"),
        Bitrate::Pcm { kbps, .. } => format!("{kbps} kbps"),
        Bitrate::Avg(k) => format!("~{k} kbps"),
        Bitrate::Unknown => "bitrate unknown".into(),
    };
    let channels = match i.channels {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        n => format!("{n} ch"),
    };
    let chips = [
        i.codec.clone(),
        bitrate,
        format!("{:.1} kHz", i.sample_rate as f64 / 1000.0),
        channels,
        fmt_short(i.duration_secs()),
        format!("{:.1} MB", i.file_size as f64 / 1_048_576.0),
    ];
    rsx! {
        div { class: "info",
            for chip in chips {
                span { class: "chip", "{chip}" }
            }
        }
    }
}

fn wave_path(cols: &[(f32, f32)]) -> String {
    let mut d = String::with_capacity(cols.len() * 24);
    for (i, &(_, hi)) in cols.iter().enumerate() {
        let _ = write!(d, "{}{i},{:.2}", if i == 0 { 'M' } else { 'L' }, 50.0 - hi * 49.0 - 0.3);
    }
    for (i, &(lo, _)) in cols.iter().enumerate().rev() {
        let _ = write!(d, "L{i},{:.2}", 50.0 - lo * 49.0 + 0.3);
    }
    d.push('Z');
    d
}

#[component]
fn Wave(scan: ScanRef, start: u64, end: u64) -> Element {
    let d = wave_path(&scan.0.peaks.columns(start, end, COLS));
    rsx! {
        svg {
            class: "wave",
            view_box: "0 0 {COLS} 100",
            preserve_aspect_ratio: "none",
            line { x1: "0", y1: "50", x2: "{COLS}", y2: "50", class: "axis" }
            path { d: "{d}" }
        }
    }
}

/// Only this component reads `pos`, so the 60 fps playhead updates touch nothing else.
#[component]
fn Playhead(start: u64, end: u64) -> Element {
    let app = use_context::<state::App>();
    let pos = (app.pos)();
    if end <= start || pos < start || pos > end {
        return rsx! {};
    }
    let pct = (pos - start) as f64 / (end - start) as f64 * 100.0;
    rsx! { div { class: "playhead", style: "left: {pct}%" } }
}

/// Split markers between `start` and `end`. In the detail view they carry the number of the
/// track that starts there.
#[component]
fn Markers(start: u64, end: u64, detail: bool) -> Element {
    let app = use_context::<state::App>();
    let cutlist = app.cutlist.read();
    let cur = (app.cur_split)();
    let Some(key) = app.selected_path().map(|p| key_of(&p)) else { return rsx! {} };
    let Some(edit) = cutlist.recordings.get(&key) else { return rsx! {} };
    let _ = app.selected.read(); // re-render on file change
    if end <= start {
        return rsx! {};
    }
    let span = (end - start) as f64;
    rsx! {
        for (i, s) in edit.splits.iter().enumerate().filter(|(_, s)| s.at >= start && s.at <= end) {
            {
                let pct = (s.at - start) as f64 / span * 100.0;
                let mut class = String::from("marker");
                class.push_str(if s.state == SplitState::Confirmed { " confirmed" } else { " suggested" });
                if cur == Some(i) {
                    class.push_str(" current");
                }
                rsx! {
                    div { key: "{i}", class: "{class}", style: "left: {pct}%" }
                }
            }
        }
    }
}

/// Each track's number and name where it starts (or at the left edge if it started earlier),
/// coloured like its split: the titles typed in the list show up on the waveform.
#[component]
fn TrackLabels(total: u64, start: u64, end: u64) -> Element {
    let app = use_context::<state::App>();
    let cutlist = app.cutlist.read();
    let _ = app.selected.read();
    let Some(edit) = app.selected_path().and_then(|p| cutlist.recordings.get(&key_of(&p))) else { return rsx! {} };
    if end <= start {
        return rsx! {};
    }
    let span = (end - start) as f64;
    let names = edit.names(total);
    let labels: Vec<(usize, &str, String, f64, f64)> = edit
        .tracks(total)
        .iter()
        .filter(|t| t.end > start && t.start < end)
        .map(|t| {
            let class = match t.index.checked_sub(1).and_then(|i| edit.splits.get(i)).map(|s| s.state) {
                None => "track-label start",
                Some(SplitState::Confirmed) => "track-label confirmed",
                Some(SplitState::Suggested) => "track-label suggested",
            };
            let text = match &names[t.index] {
                Some(name) => format!("{} · {name}", t.index + 1),
                None => format!("{}", t.index + 1),
            };
            let a = t.start.max(start) - start;
            let b = t.end.min(end) - start;
            (t.index, class, text, a as f64 / span * 100.0, (b - a) as f64 / span * 100.0)
        })
        .collect();
    rsx! {
        for (k, class, text, left, width) in labels {
            div { key: "{k}", class: "track-label-slot", style: "left: {left}%; width: {width}%",
                span { class: "{class}", title: "{text}", "{text}" }
            }
        }
    }
}

/// Transparent layer over a waveform: click/drag to seek, grab split markers to move them,
/// scroll to pan, ⌘-scroll to zoom. Positions come from client coordinates and the layer's
/// own rect, so markers under the cursor don't change the reference element.
#[component]
fn Interact(start: u64, end: u64, center: bool, edit_splits: bool) -> Element {
    let app = use_context::<state::App>();
    let mut rect = use_signal(|| (0.0f64, 1.0f64));
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let mut hover = use_signal(|| false);

    let refresh = move || {
        if let Some(m) = mounted.peek().clone() {
            spawn(async move {
                if let Ok(r) = m.get_client_rect().await {
                    rect.set((r.origin.x, r.size.width));
                }
            });
        }
    };
    let to_frame = move |cx: f64| {
        let (left, width) = *rect.peek();
        start + (((cx - left) / width.max(1.0)).clamp(0.0, 1.0) * (end - start) as f64) as u64
    };
    let grab = move |cx: f64| {
        if !edit_splits || end <= start {
            return None;
        }
        let frames_per_px = (end - start) as f64 / rect.peek().1.max(1.0);
        app.split_near(to_frame(cx), (GRAB_PX * frames_per_px) as u64)
    };

    rsx! {
        div {
            class: if hover() { "seek-layer grab" } else { "seek-layer" },
            onmounted: move |e| {
                mounted.set(Some(e.data()));
                refresh();
            },
            onresize: move |_| refresh(),
            onmousedown: move |e| {
                let x = e.client_coordinates().x;
                match grab(x) {
                    Some(i) => app.begin_drag(i),
                    None => app.seek(to_frame(x), center),
                }
            },
            onmousemove: move |e| {
                let x = e.client_coordinates().x;
                let held = e.held_buttons().contains(MouseButton::Primary);
                if app.dragging() {
                    if held {
                        app.drag_to(to_frame(x));
                    } else {
                        app.end_drag();
                    }
                } else if held {
                    app.seek(to_frame(x), false);
                } else {
                    let h = grab(x).is_some();
                    if h != *hover.peek() {
                        hover.set(h);
                    }
                }
            },
            onmouseup: move |_| app.end_drag(),
            onmouseleave: move |_| {
                app.end_drag();
                hover.set(false);
            },
            onwheel: move |e| {
                let d = e.delta().strip_units();
                let span = (end - start) as f64;
                if e.modifiers().meta() || e.modifiers().ctrl() {
                    app.zoom(if d.y > 0.0 { 1.25 } else { 0.8 });
                } else {
                    let dx = if d.x.abs() > d.y.abs() { d.x } else { d.y };
                    app.scroll_view((dx / rect.peek().1.max(1.0) * span) as i64);
                }
            },
        }
    }
}

#[component]
fn Overview(scan: ScanRef) -> Element {
    let app = use_context::<state::App>();
    let info = &scan.0.info;
    let total = info.total_samples.max(1);
    let v = (app.view)();
    let span = v.span_secs * info.sample_rate as f64;
    let left = v.start as f64 / total as f64 * 100.0;
    let width = (span / total as f64 * 100.0).min(100.0 - left);
    rsx! {
        div { class: "overview",
            Wave { scan: scan.clone(), start: 0, end: total }
            Silences { total, start: 0, end: total }
            Parts { total, start: 0, end: total, labels: false }
            AbBand { start: 0, end: total }
            Markers { start: 0, end: total, detail: false }
            div { class: "window", style: "left: {left}%; width: {width}%" }
            Playhead { start: 0, end: total }
            Interact { start: 0, end: total, center: true, edit_splits: false }
        }
    }
}

/// Tick spacing that gives at most ~10 labels across `span` seconds.
fn tick_step(span: f64) -> f64 {
    const STEPS: [f64; 13] = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
    STEPS.into_iter().find(|s| span / s <= 10.0).unwrap_or(1200.0)
}

#[component]
fn Detail(scan: ScanRef) -> Element {
    let app = use_context::<state::App>();
    let rate = scan.0.info.sample_rate as f64;
    let v = (app.view)();
    let start = v.start;
    let end = start + (v.span_secs * rate) as u64;

    let step = tick_step(v.span_secs);
    let t0 = start as f64 / rate;
    let first = (t0 / step).ceil() as i64;
    let ticks: Vec<(f64, String)> = (first..)
        .map(|k| k as f64 * step)
        .take_while(|t| *t <= t0 + v.span_secs)
        .map(|t| {
            let label = if step < 1.0 { fmt_precise(t).trim_end_matches('0').to_string() } else { fmt_short(t) };
            ((t - t0) / v.span_secs * 100.0, label)
        })
        .filter(|(pct, _)| *pct < 96.0) // a label at the far right edge would be clipped
        .collect();

    rsx! {
        div { class: "detail",
            div { class: "ruler",
                for (pct, label) in ticks {
                    div { class: "tick", style: "left: {pct}%", "{label}" }
                }
            }
            div { class: "detail-wave",
                Wave { scan: scan.clone(), start, end }
                Silences { total: scan.0.info.total_samples, start, end }
                Parts { total: scan.0.info.total_samples, start, end, labels: true }
                AbBand { start, end }
                Markers { start, end, detail: true }
                TrackLabels { total: scan.0.info.total_samples, start, end }
                Playhead { start, end }
                Interact { start, end, center: false, edit_splits: true }
            }
        }
    }
}

#[component]
fn Transport(scan: ScanRef) -> Element {
    let app = use_context::<state::App>();
    let rate = scan.0.info.sample_rate as f64;
    let total = scan.0.info.total_samples as f64 / rate;
    let pos = (app.pos)() as f64 / rate;
    let playing = (app.playing)();
    let preview = (app.preview)();
    let span = (app.view)().span_secs;
    rsx! {
        div { class: "transport",
            button {
                class: "play",
                onclick: move |_| app.toggle(),
                if playing { "❚❚" } else { "▶" }
            }
            span { class: "time", "{fmt_precise(pos)}" }
            span { class: "time dim", "/ {fmt_precise(total)}" }
            button {
                class: if preview { "preview on" } else { "preview" },
                title: "Play only what the export keeps: skip cut silence and left-out tracks (P)",
                onclick: move |_| app.toggle_preview(),
                if preview { "✂ Preview cuts: on" } else { "✂ Preview cuts" }
            }
            AbStatus {}
            span { class: "spacer" }
            button { onclick: move |_| app.zoom(2.0), "−" }
            span { class: "zoom", "{span:.0} s view" }
            button { onclick: move |_| app.zoom(0.5), "+" }
        }
    }
}

/// Review progress, the selected split, and the silence-detection settings.
#[component]
fn ReviewBar(scan: ScanRef) -> Element {
    let app = use_context::<state::App>();
    let rate = scan.0.info.sample_rate as f64;
    let cutlist = app.cutlist.read();
    let cur = (app.cur_split)();
    let _ = app.selected.read();
    let Some(edit) = app.selected_path().and_then(|p| cutlist.recordings.get(&key_of(&p)).cloned()) else {
        return rsx! {};
    };
    let (ok, todo) = edit.counts();
    let p = edit.detect;
    let selected = cur.and_then(|i| edit.splits.get(i).map(|s| (i, s.clone())));
    let done = edit.status == Status::Done;

    rsx! {
        div { class: "review",
            div { class: "review-status",
                span { class: "count", "{edit.splits.len() + 1} tracks" }
                span { class: "count ok", "{ok} confirmed" }
                span { class: if todo > 0 { "count todo" } else { "count" }, "{todo} to review" }
                match selected {
                    Some((i, s)) => rsx! {
                        span { class: "current-split",
                            "Split {i + 1} at {fmt_precise(s.at as f64 / rate)}"
                            if let Some(sil) = s.silence_secs {
                                " · {sil:.1} s silence"
                            }
                            match edit.silence_at(s.at).map(|j| edit.silences[j].keep) {
                                Some(false) => rsx! { " · silence cut (G keeps it)" },
                                Some(true) => rsx! { " · silence kept (G cuts it)" },
                                None => rsx! {},
                            }
                            if s.state == SplitState::Suggested { " · suggested" } else { " · confirmed" }
                        }
                    },
                    None if todo > 0 => rsx! { span { class: "current-split dim", "Press Tab or Enter to review the next split" } },
                    None if done => rsx! { span { class: "current-split ok", "✓ Marked done" } },
                    None => rsx! { span { class: "current-split dim", "All splits reviewed · ⌘Enter to mark done" } },
                }
            }
            div { class: "detect",
                span { class: "dim", "Silence below" }
                button { onclick: move |_| app.set_detect_params(|p| p.threshold_db -= 3.0), "−" }
                span { class: "value", "{p.threshold_db:.0} dB" }
                button { onclick: move |_| app.set_detect_params(|p| p.threshold_db = (p.threshold_db + 3.0).min(-6.0)), "+" }
                span { class: "dim", "for at least" }
                button { onclick: move |_| app.set_detect_params(|p| p.min_silence_secs = (p.min_silence_secs - 0.25).max(0.25)), "−" }
                span { class: "value", "{p.min_silence_secs:.2} s" }
                button { onclick: move |_| app.set_detect_params(|p| p.min_silence_secs += 0.25), "+" }
                button { class: "primary", onclick: move |_| app.redetect(), title: "Replace unreviewed suggestions; confirmed splits are kept", "Re-detect" }
            }
        }
    }
}

/// Tagged silence: cut from the export where it borders a track, kept (G), or inside a track.
#[component]
fn Silences(total: u64, start: u64, end: u64) -> Element {
    let app = use_context::<state::App>();
    let cutlist = app.cutlist.read();
    let _ = app.selected.read();
    let Some(edit) = app.selected_path().and_then(|p| cutlist.recordings.get(&key_of(&p))) else { return rsx! {} };
    if end <= start {
        return rsx! {};
    }
    let span = (end - start) as f64;
    let bands: Vec<(usize, &str, f64, f64)> = edit
        .silences
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s, s.end.min(total)))
        .filter(|&(_, s, e)| e > start && s.start < end)
        .map(|(i, s, e)| {
            let edge = s.start == 0 || e >= total || edit.splits.iter().any(|p| s.start < p.at && p.at < e);
            let class = match (edge, s.keep) {
                (true, false) => "silence cut",
                (true, true) => "silence kept",
                (false, _) => "silence", // inside a track: never cut
            };
            let a = s.start.max(start) - start;
            let b = e.min(end) - start;
            (i, class, a as f64 / span * 100.0, (b - a) as f64 / span * 100.0)
        })
        .collect();
    rsx! {
        for (i, class, left, width) in bands {
            div { key: "{i}", class: "{class}", style: "left: {left}%; width: {width}%" }
        }
    }
}

/// The track X and T act on (a memo, so playback only re-renders when it changes).
fn use_focus_track() -> Memo<Option<usize>> {
    let app = use_context::<state::App>();
    use_memo(move || {
        let (pos, cur, closed) = ((app.pos)(), (app.cur_split)(), (app.closed_track)());
        let _ = app.selected.read();
        let cutlist = app.cutlist.read();
        app.selected_path()
            .and_then(|p| cutlist.recordings.get(&key_of(&p)).map(|e| review::track_in_focus(e, cur, closed, pos)))
    })
}

/// Parts of the recording on the waveforms: left-out tracks in red ("✂ cut"), and the track
/// X and T act on, highlighted so it's clear what X will cut.
#[component]
fn Parts(total: u64, start: u64, end: u64, labels: bool) -> Element {
    let app = use_context::<state::App>();
    let focus = use_focus_track();
    let cutlist = app.cutlist.read();
    let _ = app.selected.read();
    let Some(edit) = app.selected_path().and_then(|p| cutlist.recordings.get(&key_of(&p))) else { return rsx! {} };
    if end <= start {
        return rsx! {};
    }
    let focus = focus();
    let names = edit.names(total);
    let span = (end - start) as f64;
    let bands: Vec<(usize, &str, String, f64, f64)> = edit
        .tracks(total)
        .iter()
        .filter(|t| t.end > start && t.start < end && (t.meta.drop || focus == Some(t.index)))
        .map(|t| {
            let a = t.start.max(start) - start;
            let b = t.end.min(end) - start;
            let (class, label) = match (t.meta.drop, focus == Some(t.index)) {
                (true, true) => ("part cut focus", "✂ cut · X to keep".to_string()),
                (true, false) => ("part cut", "✂ cut".to_string()),
                _ => {
                    let name = names[t.index].clone().unwrap_or_else(|| format!("Track {}", t.index + 1));
                    ("part focus", format!("{name} · X to cut"))
                }
            };
            (t.index, class, label, a as f64 / span * 100.0, (b - a) as f64 / span * 100.0)
        })
        .collect();
    rsx! {
        for (k, class, label, left, width) in bands {
            div { key: "{k}", class: "{class}", style: "left: {left}%; width: {width}%",
                if labels {
                    span { class: "part-label", "{label}" }
                }
            }
        }
    }
}

/// Paste a tracklist, export, and the export's progress/result.
#[component]
fn ExportBar(scan: ScanRef) -> Element {
    let app = use_context::<state::App>();
    let cutlist = app.cutlist.read();
    let _ = app.selected.read();
    let Some(path) = app.selected_path() else { return rsx! {} };
    let key = key_of(&path);
    let Some(edit) = cutlist.recordings.get(&key) else { return rsx! {} };
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let planned = plan(edit, scan.0.info.total_samples, &stem, &cutlist.export);
    let count = planned.len();
    let dropped = edit.splits.len() + 1 - count;
    let status = app.exports.read().get(&key).cloned();
    let running = status.as_ref().is_some_and(|s| s.is_active());
    let profile = cutlist.export.profile;
    let normalize = cutlist.export.normalize;
    let norm_idx = Normalize::CHOICES.iter().position(|n| *n == normalize).unwrap_or(0);
    let map = app.loudness.read().get(&path).cloned();
    let set: Option<Loudness> = map.map(|m| m.ranges(&planned.iter().map(|t| (t.start, t.end)).collect::<Vec<_>>()));
    let info = &scan.0.info;
    let facts = source_facts(&path, &scan.0);
    // What Original really writes for this file (FLAC for the audio of a video).
    let effective = profile.effective(&facts);
    let secs: f64 = planned.iter().map(|t| (t.end - t.start) as f64).sum::<f64>() / info.sample_rate as f64;
    let bytes = if effective == Profile::Original {
        // Exact for the source's own format: its share of the file.
        (info.file_size as f64 * secs / info.duration_secs().max(1e-9)) as u64
    } else {
        profile.estimate_bytes(&facts, secs)
    };
    let size = fmt_bytes(bytes);
    let warnings = profile.warnings(&facts);
    let ext = effective.extension(path.extension().and_then(|e| e.to_str()).unwrap_or("mp3")).to_string();
    let selected_idx = Profile::CHOICES.iter().position(|p| *p == profile).unwrap_or(0);

    rsx! {
        div { class: "export-bar",
            button { onclick: move |_| {
                    let mut paste = app.paste;
                    paste.set(Some(String::new()));
                },
                "Paste tracklist…"
            }
            select {
                class: "profile",
                title: "Export format",
                value: "{selected_idx}",
                onchange: move |e| {
                    if let Some(p) = e.value().parse::<usize>().ok().and_then(|i| Profile::CHOICES.get(i)) {
                        app.set_profile(*p);
                    }
                    app.focus_root();
                },
                for (i, p) in Profile::CHOICES.iter().enumerate() {
                    option { key: "{i}", value: "{i}", selected: i == selected_idx, "{p.label()}" }
                }
            }
            button {
                class: if profile.is_lossy() { "ab-button" } else { "ab-button dim" },
                title: "Loop 12 s from the playhead and switch between the original and this format (A)",
                disabled: profile == Profile::Original,
                onclick: move |_| app.ab_toggle(),
                "A/B"
                kbd { "A" }
            }
            select {
                class: "profile",
                title: if effective == Profile::Original {
                    "A byte copy can't change level; tracks get ReplayGain tags instead"
                } else {
                    "Loudness adjustment (never pushes true peak above -1 dBTP)"
                },
                disabled: effective == Profile::Original,
                value: "{norm_idx}",
                onchange: move |e| {
                    if let Some(n) = e.value().parse::<usize>().ok().and_then(|i| Normalize::CHOICES.get(i)) {
                        app.set_normalize(*n);
                    }
                    app.focus_root();
                },
                for (i, n) in Normalize::CHOICES.iter().enumerate() {
                    option { key: "{i}", value: "{i}", selected: i == norm_idx, "{n.label()}" }
                }
            }
            span { class: "dim",
                "{count} tracks · ≈ {size}"
                if dropped > 0 { " · {dropped} left out" }
                " → {stem}/*.{ext}"
            }
            match set {
                Some(Loudness { lufs: Some(l), peak_db }) => rsx! {
                    span {
                        class: if peak_db > -1.0 { "loud hot" } else { "loud" },
                        title: "Integrated loudness and true peak of the tracks being exported",
                        "{l:.1} LUFS · peak {peak_db:.1} dBTP"
                    }
                },
                Some(_) => rsx! {},
                None => rsx! { span { class: "dim", "measuring loudness…" } },
            }
            span { class: "spacer" }
            match status {
                Some(ExportStatus::Queued) => rsx! { span { class: "dim", "Queued…" } },
                Some(ExportStatus::Measuring) => rsx! { span { class: "dim", "Measuring loudness…" } },
                Some(ExportStatus::Running { done, total }) => rsx! {
                    span { class: "dim", "Exporting {done}/{total}…" }
                    div { class: "progress small", div { style: "width: {done as f64 / total.max(1) as f64 * 100.0}%" } }
                },
                Some(ExportStatus::Finished { dir, count }) => rsx! {
                    span { class: "ok", "✓ Exported {count} tracks" }
                    button { onclick: move |_| app.reveal_export(&dir), "Show in Finder" }
                },
                Some(ExportStatus::Cancelled) => rsx! { span { class: "dim", "Export cancelled" } },
                Some(ExportStatus::Failed(e)) => rsx! { span { class: "bad", title: "{e}", "Export failed" } },
                None => rsx! {},
            }
            button {
                class: "primary",
                disabled: count == 0 || running,
                onclick: move |_| app.export_current(),
                "Export"
                kbd { "⌘E" }
            }
        }
        if !warnings.is_empty() {
            div { class: "warnings",
                for (i, w) in warnings.into_iter().enumerate() {
                    div { key: "{i}", "⚠ {w}" }
                }
            }
        }
    }
}

fn fmt_bytes(b: u64) -> String {
    match b {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.0} MB", b as f64 / 1e6),
        b => format!("{:.0} kB", b as f64 / 1e3),
    }
}

/// The compared window while A/B is on.
#[component]
fn AbBand(start: u64, end: u64) -> Element {
    let app = use_context::<state::App>();
    let (a, b, ready) = match (app.ab)() {
        AbState::Preparing { start, end, .. } => (start, end, false),
        AbState::Ready { start, end, .. } => (start, end, true),
        AbState::Off => return rsx! {},
    };
    if end <= start || b <= start || a >= end {
        return rsx! {};
    }
    let span = (end - start) as f64;
    let left = (a.max(start) - start) as f64 / span * 100.0;
    let width = (b.min(end) - a.max(start)) as f64 / span * 100.0;
    rsx! {
        div { class: if ready { "ab-window" } else { "ab-window preparing" }, style: "left: {left}%; width: {width}%" }
    }
}

/// Which side of the A/B is playing.
#[component]
fn AbStatus() -> Element {
    let app = use_context::<state::App>();
    match (app.ab)() {
        AbState::Off => rsx! {},
        AbState::Preparing { profile, .. } => rsx! {
            span { class: "ab-status", "Encoding a {profile.short()} preview…" }
        },
        AbState::Ready { profile, encoded, .. } => rsx! {
            span { class: "ab-status",
                span { class: if encoded { "ab-side" } else { "ab-side on" }, "A · Original" }
                span { class: if encoded { "ab-side on enc" } else { "ab-side" }, "B · {profile.short()}" }
                span { class: "dim", " A switch · Esc stop" }
            }
        },
    }
}

#[component]
fn TrackList(scan: ScanRef) -> Element {
    let app = use_context::<state::App>();
    let current = use_focus_track();
    // Keep the current row visible.
    use_effect(move || {
        let _ = current();
        document::eval(
            "requestAnimationFrame(() => document.querySelector('.track.current')?.scrollIntoView({block: 'nearest'}))",
        );
    });

    let rate = scan.0.info.sample_rate as f64;
    let total = scan.0.info.total_samples;
    let cutlist = app.cutlist.read();
    let editing = (app.editing_title)();
    let _ = app.selected.read();
    let Some(edit) = app.selected_path().and_then(|p| cutlist.recordings.get(&key_of(&p)).cloned()) else {
        return rsx! {};
    };
    let names = edit.names(total);
    let tracks: Vec<_> = edit
        .tracks(total)
        .into_iter()
        .map(|t| (t.index, t.audio_start, t.audio_end, t.meta.clone(), names[t.index].clone().unwrap_or_default()))
        .collect();
    let count = tracks.len();
    let current = current();
    let map = app.selected_path().and_then(|p| app.loudness.read().get(&p).cloned());

    rsx! {
        div { class: "tracks",
            for (k, a, b, meta, name) in tracks {
                {
                    // Track k starts at split k-1.
                    let state = k.checked_sub(1).and_then(|i| edit.splits.get(i)).map(|s| s.state);
                    let (badge, badge_class) = match state {
                        None => ("start", "badge"),
                        Some(SplitState::Confirmed) => ("confirmed", "badge ok"),
                        Some(SplitState::Suggested) => ("suggested", "badge todo"),
                    };
                    let mut class = String::from("track");
                    if current == Some(k) {
                        class.push_str(" current");
                    }
                    if meta.drop || b <= a {
                        class.push_str(" dropped-row");
                    }
                    rsx! {
                        div {
                            key: "{k}",
                            class: "{class}",
                            onclick: move |_| app.play_track(k),
                            span { class: "num", "{k + 1}" }
                            span { class: "t", "{fmt_precise(a as f64 / rate)}" }
                            span { class: "len", "{fmt_short((b - a) as f64 / rate)}" }
                            if editing == Some(k) {
                                TitleEditor {
                                    key: "{k}",
                                    track: k,
                                    count,
                                    initial: meta.title.clone(),
                                    suggested: name.clone(),
                                }
                            } else {
                                div { class: "title-cell",
                                    span {
                                        class: if meta.title.is_empty() { "title empty" } else { "title" },
                                        if !meta.title.is_empty() {
                                            "{meta.title}"
                                        } else if !name.is_empty() {
                                            "{name}"
                                        } else {
                                            "untitled"
                                        }
                                    }
                                    button {
                                        class: "rename",
                                        title: "Rename (T)",
                                        onclick: move |e| {
                                            e.stop_propagation();
                                            app.edit_title(Some(k));
                                        },
                                        "✎"
                                    }
                                }
                            }
                            {
                                match map.as_ref().map(|m| m.range(a, b)) {
                                    Some(Loudness { lufs: Some(l), peak_db }) => rsx! {
                                        span { class: "lufs", title: "Integrated loudness", "{l:.1}" }
                                        span {
                                            class: if peak_db > -1.0 { "peak hot" } else { "peak" },
                                            title: "True peak (dBTP)",
                                            "{peak_db:.1}"
                                        }
                                    },
                                    Some(_) => rsx! { span { class: "lufs dim", "silent" } span { class: "peak" } },
                                    None => rsx! { span { class: "lufs dim", "…" } span { class: "peak" } },
                                }
                            }
                            span { class: "{badge_class}", "{badge}" }
                            button {
                                class: "drop-toggle",
                                title: if meta.drop { "Include in the export (X)" } else { "Leave out of the export (X)" },
                                onclick: move |e| {
                                    e.stop_propagation();
                                    app.toggle_drop(Some(k));
                                },
                                if meta.drop { "↺" } else { "✕" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Inline title field. Enter saves and moves on to the next track, Esc cancels.
#[component]
fn TitleEditor(track: usize, count: usize, initial: String, suggested: String) -> Element {
    let app = use_context::<state::App>();
    let mut draft = use_signal(|| initial.clone());
    let mut closed = use_signal(|| false);
    let suggestion = use_signal(|| suggested.clone());
    // `accept`: an empty field takes the suggested name (Tab), instead of staying untitled.
    let mut close = move |save: bool, accept: bool, next: Option<usize>| {
        if closed() {
            return;
        }
        closed.set(true);
        if save {
            let typed = draft();
            let title = if accept && typed.trim().is_empty() { suggestion() } else { typed };
            app.set_title(track, title);
        }
        let mut editing = app.editing_title;
        editing.set(next);
        if next.is_none() {
            app.focus_root();
        }
    };
    rsx! {
        input {
            class: "title-input",
            value: "{draft}",
            placeholder: if suggested.is_empty() { "Title".to_string() } else { format!("{suggested} (Tab to accept)") },
            onmounted: move |e| async move {
                let _ = e.set_focus(true).await;
            },
            oninput: move |e| draft.set(e.value()),
            onclick: move |e| e.stop_propagation(),
            onkeydown: move |e| {
                e.stop_propagation();
                let next = (track + 1 < count).then_some(track + 1);
                match e.key() {
                    Key::Enter => close(true, false, next),
                    // Tab: keep what's typed, or take the suggested name; then the next (⇧ previous) track.
                    Key::Tab => {
                        e.prevent_default();
                        close(true, true, if e.modifiers().shift() { track.checked_sub(1) } else { next });
                    }
                    Key::Escape => close(false, false, None),
                    _ => {}
                }
            },
            onblur: move |_| close(true, false, None),
        }
    }
}

/// Paste a tracklist; titles go to the kept tracks in order.
#[component]
fn PasteDialog() -> Element {
    let app = use_context::<state::App>();
    let Some(text) = (app.paste)() else { return rsx! {} };
    let titles = tracklist::parse(&text);
    let cutlist = app.cutlist.read();
    let kept = app
        .selected_path()
        .and_then(|p| cutlist.recordings.get(&key_of(&p)))
        .map(|e| e.tracks(u64::MAX).iter().filter(|t| t.exported()).count())
        .unwrap_or(0);
    let mut paste = app.paste;
    let close = move || {
        let mut paste = app.paste;
        paste.set(None);
        app.focus_root();
    };
    let apply = move || {
        if let Some(text) = app.paste.peek().clone() {
            app.apply_tracklist(&text);
        }
        close();
    };
    let preview: Vec<(usize, String)> = titles.iter().take(8).cloned().enumerate().collect();

    rsx! {
        div { class: "modal-backdrop", onclick: move |_| close(),
            div { class: "modal", onclick: move |e| e.stop_propagation(),
                h2 { "Paste tracklist" }
                p { class: "dim",
                    "One title per line. Track numbers, timestamps and durations are removed. "
                    "Titles go to the kept tracks in order."
                }
                textarea {
                    value: "{text}",
                    rows: "10",
                    placeholder: "1. Intro\n2. First song (4:12)\n…",
                    onmounted: move |e| async move {
                        let _ = e.set_focus(true).await;
                    },
                    oninput: move |e| paste.set(Some(e.value())),
                    onkeydown: move |e| {
                        e.stop_propagation();
                        let m = e.modifiers();
                        match e.key() {
                            Key::Escape => close(),
                            Key::Enter if m.meta() || m.ctrl() => apply(),
                            _ => {}
                        }
                    },
                }
                div { class: if titles.len() == kept { "match ok" } else { "match todo" },
                    "{titles.len()} titles for {kept} kept tracks"
                    if titles.len() > kept { " — the extra titles will be ignored" }
                    if titles.len() < kept && !titles.is_empty() { " — the last tracks keep their titles" }
                }
                ol { class: "preview",
                    for (i, t) in preview {
                        li { key: "{i}", "{t}" }
                    }
                    if titles.len() > 8 { li { class: "dim", "…" } }
                }
                div { class: "modal-actions",
                    button { onclick: move |_| close(), "Cancel" }
                    button { class: "primary", disabled: titles.is_empty(), onclick: move |_| apply(),
                        "Apply titles"
                        kbd { "⌘Enter" }
                    }
                }
            }
        }
    }
}

#[component]
fn Keys() -> Element {
    let review = [
        ("Tab ⇧Tab", "next / prev split"),
        ("Enter", "keep + next"),
        ("⌫", "delete + next"),
        (", .", "nudge 10 ms (⇧ 100 ms)"),
        ("S", "snap to silence"),
        ("M", "add split"),
        ("C", "listen again"),
        ("⌘Z", "undo"),
        ("⌘Enter", "file done"),
    ];
    let tracks = [
        ("T", "title"),
        ("R", "rename file"),
        ("P", "preview cuts"),
        ("X", "cut / keep this part"),
        ("G", "cut / keep silence"),
        ("A", "A/B compare"),
        ("F", "flag file"),
        ("⌘E", "export"),
        ("⇧⌘E", "export done files"),
    ];
    let nav = [
        ("Space", "play / pause"),
        ("← →", "±5 s (⇧ 0.5 s, ⌥ 10 ms)"),
        ("↑ ↓", "prev / next file"),
        ("+ −", "zoom"),
        ("drag marker", "move split"),
    ];
    rsx! {
        div { class: "keys",
            for (k, what) in review {
                span { kbd { "{k}" } " {what}" }
            }
        }
        div { class: "keys",
            for (k, what) in tracks.into_iter().chain(nav) {
                span { kbd { "{k}" } " {what}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_readable() {
        assert_eq!(tick_step(2.0), 0.2);
        assert_eq!(tick_step(20.0), 2.0);
        assert_eq!(tick_step(600.0), 60.0);
    }

    #[test]
    fn wave_path_is_closed() {
        let d = wave_path(&[(0.0, 0.5), (-0.5, 0.0)]);
        assert!(d.starts_with("M0,") && d.ends_with('Z'));
    }
}
