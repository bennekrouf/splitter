// Release builds on Windows are GUI apps: no console window next to the main one.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ab;
mod download;
mod exporting;
mod review;
mod state;
mod views;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};

/// Where the webview keeps its data. The default is next to the executable, which isn't
/// writable when installed for all users (WebView2 then fails to start on Windows).
fn webview_data_dir() -> std::path::PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join("Splitter")
}

fn main() {
    let cfg = Config::new()
        .with_data_directory(webview_data_dir())
        .with_window(
            WindowBuilder::new()
                .with_title("Splitter")
                .with_inner_size(LogicalSize::new(1280.0, 820.0))
                .with_window_icon(window_icon()),
        )
        .with_background_color((24, 24, 27, 255));
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(views::App);
}

/// The window icon, decoded from the embedded logo.
///
/// build.rs embeds `assets/icon.ico` into the .exe, which covers the Start menu and
/// shortcuts, but the window itself (title bar, alt-tab, taskbar button on Windows and
/// Linux) only shows what the app sets at runtime. Downscaled to 64 px: the platform
/// gets this one bitmap for every size, and a 1024 px source squeezed into a 16 px title
/// bar looks muddy.
fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
    const SIZE: u32 = 64;
    let img = image::load_from_memory(ICON_PNG).ok()?.resize_exact(SIZE, SIZE, image::imageops::FilterType::Lanczos3);
    dioxus::desktop::tao::window::Icon::from_rgba(img.into_rgba8().into_raw(), SIZE, SIZE).ok()
}
