mod ab;
mod exporting;
mod review;
mod state;
mod views;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};

fn main() {
    let cfg = Config::new()
        .with_window(WindowBuilder::new().with_title("Splitter").with_inner_size(LogicalSize::new(1280.0, 820.0)))
        .with_background_color((24, 24, 27, 255));
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(views::App);
}
