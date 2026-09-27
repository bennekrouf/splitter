//! Build script: embeds the Windows app icon into the .exe resource, and dates the build.
//!
//! Looks for `assets/icon.ico` (release CI generates it from `assets/icon.png`). If
//! present, the icon is compiled into the executable so it shows in the taskbar, Start
//! menu and Explorer. If absent, the build still succeeds, icon-less, with a
//! `cargo:warning` so the gap is visible in the build log.
//!
//! `SPLITTER_RELEASE_DATE` (`YYYY-MM-DD`) is what a Pro licence's `updates_until` is
//! compared with: taken from the environment if set, else the date of the commit being
//! built (the release commit, in CI), else left empty (unknown: every licence covers it).
//! `SPLITTER_LICENSE_PUBLIC_KEY` is read by the app itself with `option_env!`; it is
//! listed here so changing it rebuilds.
//!
//! The Windows part is a no-op on other targets.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SPLITTER_RELEASE_DATE");
    println!("cargo:rerun-if-env-changed=SPLITTER_LICENSE_PUBLIC_KEY");
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    let date = std::env::var("SPLITTER_RELEASE_DATE").ok().filter(|d| !d.is_empty()).or_else(commit_date);
    println!("cargo:rustc-env=SPLITTER_RELEASE_DATE={}", date.unwrap_or_default());

    #[cfg(target_os = "windows")]
    {
        let icon_path = std::path::Path::new("assets/icon.ico");
        if !icon_path.exists() {
            println!(
                "cargo:warning=assets/icon.ico not found — the .exe will ship without an embedded icon. \
                 Generate it from assets/icon.png (see the release workflow)."
            );
            return;
        }

        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "Splitter");
        res.set("ProductName", "Splitter");
        res.set("CompanyName", "Bennekrouf");
        res.set("LegalCopyright", "© Bennekrouf");
        if let Err(e) = res.compile() {
            // rc.exe / windres isn't on every Windows runner; warn and ship icon-less rather
            // than failing the build.
            println!("cargo:warning=Failed to embed Windows icon resource: {e} (the build continues without it)");
        }
    }
}

/// The committer date of HEAD, `YYYY-MM-DD`.
fn commit_date() -> Option<String> {
    let out = std::process::Command::new("git").args(["log", "-1", "--format=%cs"]).output().ok()?;
    let date = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && date.len() == 10).then_some(date)
}
