//! yt-dlp and Deno (the JavaScript runtime yt-dlp needs for YouTube), installed into the app's
//! data folder on first use, so nothing has to be installed by hand.
//!
//! Both come from their projects' GitHub releases and are checked against the SHA-256 sums
//! published with them. yt-dlp's standalone builds carry their own Python. It updates itself
//! (`yt-dlp -U`) at most once a day, since YouTube changes often and old versions stop working.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const YT_DLP_RELEASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const DENO_RELEASE: &str = "https://github.com/denoland/deno/releases/latest/download";
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);

pub struct Tools {
    pub yt_dlp: PathBuf,
    pub deno: PathBuf,
}

/// %LOCALAPPDATA%\Splitter\tools, ~/Library/Application Support/Splitter/tools, ~/.local/share/Splitter/tools.
pub fn dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Splitter").join("tools")
}

/// A command that doesn't open a console window next to the app on Windows.
pub fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Install whatever is missing into `dir` and update yt-dlp if it's due. `progress` gets the
/// fraction fetched while installing; `cancelled` is polled between chunks.
pub fn ensure(dir: &Path, progress: &mut dyn FnMut(f32), cancelled: &dyn Fn() -> bool) -> Result<Tools, String> {
    let exe = std::env::consts::EXE_SUFFIX;
    let tools = Tools { yt_dlp: dir.join(format!("yt-dlp{exe}")), deno: dir.join(format!("deno{exe}")) };
    let missing = [&tools.yt_dlp, &tools.deno].iter().filter(|p| !p.is_file()).count();
    if missing > 0 {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    let done = std::cell::Cell::new(0);
    let mut step = |p: f32| progress((done.get() as f32 + p) / missing as f32);
    if !tools.yt_dlp.is_file() {
        let asset = yt_dlp_asset().ok_or("Opening URLs isn't supported on this platform")?;
        let sums = get_text(&format!("{YT_DLP_RELEASE}/SHA2-256SUMS"))?;
        let sum = sums
            .lines()
            .find_map(|l| l.split_once("  ").filter(|(_, name)| name.trim() == asset).map(|(h, _)| h.to_owned()))
            .ok_or_else(|| format!("No checksum published for {asset}"))?;
        let tmp = fetch_verified(&format!("{YT_DLP_RELEASE}/{asset}"), &sum, dir, &mut step, cancelled)?;
        install(&tmp, &tools.yt_dlp)?;
        stamp_checked(dir);
        done.set(1);
    }
    if !tools.deno.is_file() {
        let asset = deno_asset().ok_or("Opening URLs isn't supported on this platform")?;
        let sum = get_text(&format!("{DENO_RELEASE}/{asset}.sha256sum"))?;
        let sum = find_sha256(&sum).ok_or_else(|| format!("No checksum published for {asset}"))?;
        let zip = fetch_verified(&format!("{DENO_RELEASE}/{asset}"), &sum, dir, &mut step, cancelled)?;
        let tmp = dir.join("deno.unzipped");
        let unzipped = unzip_one(&zip, &format!("deno{exe}"), &tmp);
        let _ = std::fs::remove_file(&zip);
        unzipped?;
        install(&tmp, &tools.deno)?;
    }
    if missing == 0 && update_due(dir) {
        // Best effort: on failure (offline, rate limited) the installed version is used.
        let _ = command(&tools.yt_dlp).args(["-U", "--ignore-config"]).output();
        stamp_checked(dir);
    }
    Ok(tools)
}

fn yt_dlp_asset() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => "yt-dlp_macos", // universal
        ("windows", _) => "yt-dlp.exe", // x64; Windows on ARM runs it emulated
        ("linux", "x86_64") => "yt-dlp_linux",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        _ => return None,
    })
}

fn deno_asset() -> Option<String> {
    let arch = std::env::consts::ARCH;
    if !matches!(arch, "x86_64" | "aarch64") {
        return None;
    }
    let target = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        "linux" => "unknown-linux-gnu",
        _ => return None,
    };
    Some(format!("deno-{arch}-{target}.zip"))
}

fn get_text(url: &str) -> Result<String, String> {
    ureq::get(url)
        .call()
        .and_then(|mut r| r.body_mut().read_to_string())
        .map_err(|e| format!("Could not download {url}: {e}"))
}

/// The first 64-hex-digit word in `text` (Deno's sum files differ between platforms).
fn find_sha256(text: &str) -> Option<String> {
    text.split_whitespace().find(|w| w.len() == 64 && w.chars().all(|c| c.is_ascii_hexdigit())).map(str::to_lowercase)
}

/// Download `url` into `dir` and check its SHA-256. Returns the temporary file.
fn fetch_verified(
    url: &str,
    sha256: &str,
    dir: &Path,
    progress: &mut dyn FnMut(f32),
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let err = |e: &dyn std::fmt::Display| format!("Could not download {url}: {e}");
    let mut resp = ureq::get(url).call().map_err(|e| err(&e))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    let name = url.rsplit('/').next().unwrap_or("download");
    let tmp = dir.join(format!("{name}.part"));
    let result = (|| {
        let mut out = File::create(&tmp).map_err(|e| err(&e))?;
        let mut body = resp.body_mut().as_reader();
        let mut hasher = Sha256::new();
        let (mut buf, mut got) = (vec![0u8; 1 << 16], 0u64);
        loop {
            if cancelled() {
                return Err("cancelled".to_string());
            }
            let n = body.read(&mut buf).map_err(|e| err(&e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| err(&e))?;
            got += n as u64;
            if let Some(t) = total.filter(|&t| t > 0) {
                progress((got as f64 / t as f64).min(1.0) as f32);
            }
        }
        let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if actual != sha256.to_lowercase() {
            return Err(format!("{name} didn't match its published checksum; try again"));
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn unzip_one(zip: &Path, entry: &str, dst: &Path) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("Could not unpack {}: {e}", zip.display());
    let mut archive = zip::ZipArchive::new(File::open(zip).map_err(|e| err(&e))?).map_err(|e| err(&e))?;
    let mut file = archive.by_name(entry).map_err(|e| err(&e))?;
    let mut out = File::create(dst).map_err(|e| err(&e))?;
    std::io::copy(&mut file, &mut out).map_err(|e| err(&e))?;
    Ok(())
}

/// Make `tmp` executable and move it into place.
fn install(tmp: &Path, dst: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(tmp, dst).map_err(|e| format!("Could not install {}: {e}", dst.display()))
}

fn stamp_checked(dir: &Path) {
    let _ = std::fs::write(dir.join("yt-dlp.checked"), b"");
}

fn update_due(dir: &Path) -> bool {
    let checked = std::fs::metadata(dir.join("yt-dlp.checked")).and_then(|m| m.modified());
    checked.map_or(true, |t| SystemTime::now().duration_since(t).map_or(true, |age| age > UPDATE_EVERY))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_from_sum_files() {
        let h = "a".repeat(64);
        assert_eq!(find_sha256(&format!("{h}  deno-aarch64-apple-darwin.zip")), Some(h.clone()));
        // Windows sum files are PowerShell's Get-FileHash list, with CRLF.
        let ps =
            format!("\r\nAlgorithm : SHA256\r\nHash      : {}\r\nPath      : C:\\a\\deno.zip\r\n", h.to_uppercase());
        assert_eq!(find_sha256(&ps), Some(h));
        assert_eq!(find_sha256("nothing here"), None);
    }

    #[test]
    fn assets_exist_for_this_platform() {
        assert!(yt_dlp_asset().is_some());
        assert!(deno_asset().is_some());
    }
}
