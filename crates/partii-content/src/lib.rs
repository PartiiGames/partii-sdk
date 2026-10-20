//! Live content: the files a developer pushed with `partii content push`
//! (levels, tuning, scripts), mirrored into a local folder so the game reads
//! plain files. Only files whose sha256 changed are downloaded; files the
//! channel dropped are removed. Every download is checked against its hash,
//! and the folder is updated only after every file arrived, so a game never
//! sees half a version. Offline, the last synced version stays in place.
//!
//! ```no_run
//! let report = partii_content::sync(&partii_content::ContentConfig {
//!     edge: "https://<your platform>".into(),
//!     key: "pk_mystudio_game_...".into(),
//!     game: "mygame".into(),
//!     channel: "stable".into(),
//!     dir: "/path/to/save/folder/content".into(),
//! });
//! // report.version, report.changed …; then read files under `dir`.
//! ```
//!
//! `sync` blocks on the network; call it from a background thread.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use partii_client::{Client, Reply};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct ContentConfig {
    pub edge: String,
    /// The game's publishable key.
    pub key: String,
    pub game: String,
    /// "stable" unless you push to other channels.
    pub channel: String,
    /// The local mirror (created if missing).
    pub dir: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub game: String,
    pub channel: String,
    pub version: String,
    pub files: BTreeMap<String, FileEntry>,
}

#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    /// The version now in `dir` (None: nothing ever synced).
    pub version: Option<String>,
    /// Paths written or replaced this time.
    pub changed: Vec<String>,
    /// Paths removed because the channel dropped them.
    pub removed: Vec<String>,
    /// Why the platform's copy couldn't be fetched (the old copy stays).
    pub error: Option<String>,
}

const STATE: &str = ".partii-content.json";

/// The manifest of what's in `dir` now.
pub fn local(dir: &Path) -> Option<Manifest> {
    std::fs::read_to_string(dir.join(STATE))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A content path that can't escape `dir`.
fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

/// Bring `dir` up to the channel's current version.
pub fn sync(cfg: &ContentConfig) -> SyncReport {
    let have = local(&cfg.dir);
    let mut report = SyncReport {
        version: have.as_ref().map(|m| m.version.clone()),
        ..Default::default()
    };
    let c = Client {
        base: cfg.edge.clone(),
        key: Some(cfg.key.clone()),
        ..Default::default()
    };
    let want: Manifest = match c.json(
        "contentManifest",
        &[
            ("game", cfg.game.as_str()),
            ("channel", cfg.channel.as_str()),
        ],
        None,
    ) {
        Ok(v) if v["ok"] == true => match serde_json::from_value(v["manifest"].clone()) {
            Ok(m) => m,
            Err(e) => {
                report.error = Some(format!("bad manifest: {e}"));
                return report;
            }
        },
        Ok(v) if v["error"]["code"] == "not_found" => Manifest {
            game: cfg.game.clone(),
            channel: cfg.channel.clone(),
            ..Default::default()
        },
        Ok(v) => {
            report.error = Some(
                v["error"]["message"]
                    .as_str()
                    .unwrap_or("manifest refused")
                    .to_string(),
            );
            return report;
        }
        Err(e) => {
            report.error = Some(e.message);
            return report;
        }
    };
    if have.as_ref() == Some(&want) {
        return report;
    }
    let old = have.unwrap_or_default();
    // Fetch every changed file first; touch nothing until all arrived.
    let mut fetched: Vec<(String, Vec<u8>)> = Vec::new();
    for (path, entry) in &want.files {
        if !safe(path) {
            report.error = Some(format!("unsafe content path {path}"));
            return report;
        }
        if old.files.get(path) == Some(entry) && cfg.dir.join(path).exists() {
            continue;
        }
        let op = partii_client::op("contentFile").expect("contentFile op");
        let args: BTreeMap<String, String> = [
            ("game".to_string(), cfg.game.clone()),
            ("sha".to_string(), entry.sha256.clone()),
        ]
        .into();
        let bytes = match c.call(op, &args, None, None, false) {
            Ok(Reply::Bytes { status: 200, bytes }) => bytes,
            Ok(other) => {
                report.error = Some(
                    format!("{path}: unexpected reply {other:?}")
                        .chars()
                        .take(200)
                        .collect(),
                );
                return report;
            }
            Err(e) => {
                report.error = Some(format!("{path}: {}", e.message));
                return report;
            }
        };
        if sha_hex(&bytes) != entry.sha256 {
            report.error = Some(format!("{path}: hash mismatch"));
            return report;
        }
        fetched.push((path.clone(), bytes));
    }
    for (path, bytes) in fetched {
        let dest = cfg.dir.join(&path);
        if let Some(d) = dest.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = dest.with_extension("partii-tmp");
        if std::fs::write(&tmp, &bytes)
            .and_then(|_| std::fs::rename(&tmp, &dest))
            .is_err()
        {
            report.error = Some(format!("{path}: could not write"));
            return report;
        }
        report.changed.push(path);
    }
    for path in old
        .files
        .keys()
        .filter(|p| !want.files.contains_key(*p) && safe(p))
    {
        let _ = std::fs::remove_file(cfg.dir.join(path));
        report.removed.push(path.clone());
    }
    let _ = std::fs::create_dir_all(&cfg.dir);
    if let Ok(s) = serde_json::to_string_pretty(&want) {
        let _ = std::fs::write(cfg.dir.join(STATE), s);
    }
    report.version = (!want.version.is_empty()).then(|| want.version.clone());
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_paths_stay_inside_the_folder() {
        for ok in ["levels/1.json", "brains/hunt.py", "a"] {
            assert!(safe(ok), "{ok}");
        }
        for bad in ["", "/etc/passwd", "../x", "a/../../b", "a//b", "./a", "a b"] {
            assert!(!safe(bad), "{bad}");
        }
    }

    #[test]
    fn sha_matches_the_platform() {
        assert_eq!(
            sha_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
