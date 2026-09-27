//! The Splitter Pro licence on this computer: the pasted key, kept in the config folder,
//! and what it unlocks for this build. The check itself is offline, in
//! `splitter_core::license`.

use base64::Engine;
use splitter_core::license::{self, License, LicenseError};
use std::path::PathBuf;

/// Where "Buy Splitter Pro" leads.
pub const BUY_URL: &str = "https://mayorana.ch/en/apps/splitter";

/// The public half of mayorana.ch's licence signing key (32 bytes, standard base64),
/// built in by the release workflow from the `SPLITTER_LICENSE_PUBLIC_KEY` variable.
const PUBLIC_KEY: Option<&str> = option_env!("SPLITTER_LICENSE_PUBLIC_KEY");

/// This build's release date (`YYYY-MM-DD`), set by build.rs. Empty when unknown, which
/// every licence covers.
const RELEASE_DATE: &str = env!("SPLITTER_RELEASE_DATE");

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// No licence on this computer.
    Free,
    /// Licensed, and this build is covered.
    Pro(License),
    /// Licensed, but this build was released after the licence's updates ended: it keeps
    /// unlocking the releases up to that day.
    Renew(License),
    /// A build without the public key (a local build): licences can't be checked.
    Unavailable,
}

impl Status {
    /// Whether exports write every track. A build that can't check licences isn't
    /// limited either: that is a local build from source, or a release missing its key,
    /// and neither should lock out someone who paid.
    pub fn exports_all(&self) -> bool {
        matches!(self, Status::Pro(_) | Status::Unavailable)
    }
}

fn public_key() -> Option<[u8; 32]> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(PUBLIC_KEY?.trim()).ok()?;
    bytes.try_into().ok()
}

fn file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("splitter").join("licence"))
}

fn status_of(license: License) -> Status {
    if license.covers(RELEASE_DATE) {
        Status::Pro(license)
    } else {
        Status::Renew(license)
    }
}

/// The licence saved on this computer, checked again at every start: a key that no longer
/// verifies (the file was edited) counts as none.
pub fn current() -> Status {
    let Some(public) = public_key() else { return Status::Unavailable };
    let Some(key) = file().and_then(|f| std::fs::read_to_string(f).ok()) else { return Status::Free };
    match license::verify(&key, &public) {
        Ok(l) => status_of(l),
        Err(_) => Status::Free,
    }
}

/// Checks `key` and, if it is a Splitter licence, saves it for the next starts.
pub fn activate(key: &str) -> Result<Status, String> {
    let public = public_key().ok_or("This build of Splitter can't check licences. Download it from mayorana.ch.")?;
    let license = license::verify(key, &public).map_err(|e: LicenseError| e.to_string())?;
    let f = file().ok_or("No settings folder to keep the licence in.")?;
    let key: String = key.chars().filter(|c| !c.is_whitespace()).collect();
    std::fs::create_dir_all(f.parent().unwrap())
        .and_then(|_| std::fs::write(&f, key))
        .map_err(|e| format!("The licence couldn't be saved: {e}"))?;
    Ok(status_of(license))
}

/// Removes the licence from this computer (to move it to another one).
pub fn deactivate() {
    if let Some(f) = file() {
        let _ = std::fs::remove_file(f);
    }
}

/// This build's release date, as shown next to a licence that needs renewing.
pub fn release_date() -> &'static str {
    RELEASE_DATE
}
