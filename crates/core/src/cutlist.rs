//! `splitter.cutlist.json`: all edits for a folder, keyed by file name so the folder can move.

use crate::edit::RecordingEdit;
use crate::export::ExportSettings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE_NAME: &str = "splitter.cutlist.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cutlist {
    pub version: u32,
    #[serde(default)]
    pub export: ExportSettings,
    pub recordings: BTreeMap<String, RecordingEdit>,
}

impl Default for Cutlist {
    fn default() -> Self {
        Self { version: 1, export: ExportSettings::default(), recordings: BTreeMap::new() }
    }
}

impl Cutlist {
    /// The folder's cutlist, or an empty one if there is none yet.
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        match std::fs::read(dir.join(FILE_NAME)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Write atomically, so a crash mid-save never leaves a truncated cutlist.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = dir.join(format!(".{FILE_NAME}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, dir.join(FILE_NAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Split;

    #[test]
    fn round_trips() {
        let dir = std::env::temp_dir().join(format!("splitter-cutlist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Cutlist::load(&dir).unwrap(), Cutlist::default());

        let mut c = Cutlist::default();
        let mut e = RecordingEdit::default();
        e.insert(Split::confirmed(44100));
        c.recordings.insert("set.mp3".into(), e);
        c.save(&dir).unwrap();
        assert_eq!(Cutlist::load(&dir).unwrap(), c);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
