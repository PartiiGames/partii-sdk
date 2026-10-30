//! Reports to the game's developer from inside the game (an in-game bug key): one call that
//! sends the report (`gamesReportsCreate`), starts each file (`gamesReportsAttach`) and PUTs
//! its bytes straight to storage on the presigned URL, with the sha256 the URL was signed for.
//!
//! ```no_run
//! use partii_client::{reports, Client};
//! let client = Client { base: "https://api.partii.games".into(), key: Some("pk_…".into()), player_token: Some("pt_…".into()), ..Default::default() };
//! let sent = reports::submit(&client, "bouncy-blobs", &reports::Report {
//!     kind: reports::Kind::Bug,
//!     note: "fell through the floor on level 3".into(),
//!     build: reports::Build { version: Some("1.4.2".into()), commit: Some("abc123".into()), platform: Some("macos".into()), ..Default::default() },
//!     meta: serde_json::json!({ "level": 3, "seed": 42 }),
//!     files: vec![reports::File { name: "inputs.bin".into(), content_type: "application/octet-stream".into(), data: vec![1, 2, 3] }],
//! }).unwrap();
//! println!("report {} ({} files, {} failed)", sent.id, sent.attachments.len(), sent.failed.len());
//! ```
//!
//! The report is in once `submit` returns `Ok`; a file that fails to upload is listed in
//! `failed` rather than failing the report. Files: image/png and image/jpeg up to 10 MB;
//! text/plain, application/json and application/octet-stream up to 16 MB; at most 5. It's a
//! blocking call: run it off the game's main thread.

use crate::{CallError, Client};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// What the report is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Bug,
    Feedback,
    Crash,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Bug => "bug",
            Kind::Feedback => "feedback",
            Kind::Crash => "crash",
        }
    }
}

/// The build the player ran (each a short string, at most 128 characters; all optional).
#[derive(Debug, Clone, Default)]
pub struct Build {
    pub version: Option<String>,
    pub commit: Option<String>,
    pub platform: Option<String>,
    pub device: Option<String>,
    pub os: Option<String>,
}

/// A file to attach: a screenshot, a log, an input recording.
#[derive(Debug, Clone, Default)]
pub struct File {
    pub name: String,
    /// image/png, image/jpeg, text/plain, application/json or application/octet-stream.
    pub content_type: String,
    pub data: Vec<u8>,
}

/// One report.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub kind: Kind,
    /// At most 5,000 characters.
    pub note: String,
    pub build: Build,
    /// Any JSON object (at most 64 KB); `Value::Null` for none.
    pub meta: Value,
    pub files: Vec<File>,
}

/// What `submit` did.
#[derive(Debug)]
pub struct Sent {
    /// The report's id (gr_…).
    pub id: String,
    /// The report as the API returned it.
    pub report: Value,
    /// The files that were stored ({id, name, type, size, sha256}).
    pub attachments: Vec<Value>,
    /// The files that weren't, with why.
    pub failed: Vec<(String, ReportError)>,
}

/// Why a call failed: the API's error code (e.g. rate_limited, too_large), or network.
#[derive(Debug, Clone)]
pub struct ReportError {
    pub code: String,
    pub message: String,
    pub hint: String,
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ReportError {}

impl From<CallError> for ReportError {
    fn from(e: CallError) -> Self {
        ReportError {
            code: e.code.to_string(),
            message: e.message,
            hint: e.hint,
        }
    }
}

/// The JSON body `gamesReportsCreate` takes (no files).
pub fn body(report: &Report) -> Value {
    let mut build = Map::new();
    for (k, v) in [
        ("version", &report.build.version),
        ("commit", &report.build.commit),
        ("platform", &report.build.platform),
        ("device", &report.build.device),
        ("os", &report.build.os),
    ] {
        if let Some(v) = v {
            build.insert(k.into(), Value::String(v.clone()));
        }
    }
    let mut b = json!({ "kind": report.kind.as_str(), "note": report.note, "build": build });
    if !report.meta.is_null() {
        b["meta"] = report.meta.clone();
    }
    b
}

/// The hex sha256 of some bytes.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn ok(v: Value) -> Result<Value, ReportError> {
    if v["ok"] == true {
        return Ok(v);
    }
    let s = |k: &str| v["error"][k].as_str().unwrap_or_default().to_string();
    Err(ReportError {
        code: if s("code").is_empty() {
            "error".into()
        } else {
            s("code")
        },
        message: s("message"),
        hint: s("hint"),
    })
}

/// Send a report to the game's developer, then its files. Needs the client's publishable key
/// and player token (a device guest's is fine).
pub fn submit(client: &Client, game: &str, report: &Report) -> Result<Sent, ReportError> {
    let created = ok(client.json("gamesReportsCreate", &[("game", game)], Some(body(report)))?)?;
    let id = created["report"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut sent = Sent {
        id: id.clone(),
        report: created["report"].clone(),
        attachments: Vec::new(),
        failed: Vec::new(),
    };
    for f in &report.files {
        match attach(client, game, &id, f) {
            Ok(a) => sent.attachments.push(a),
            Err(e) => sent.failed.push((f.name.clone(), e)),
        }
    }
    Ok(sent)
}

fn attach(client: &Client, game: &str, id: &str, f: &File) -> Result<Value, ReportError> {
    let start = ok(client.json(
        "gamesReportsAttach",
        &[("game", game), ("id", id)],
        Some(json!({ "name": f.name, "type": f.content_type, "size": f.data.len(), "sha256": sha256_hex(&f.data) })),
    )?)?;
    let url = start["upload"]["url"].as_str().unwrap_or_default();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent.put(url);
    if let Some(h) = start["upload"]["headers"].as_object() {
        for (k, v) in h {
            req = req.header(k.as_str(), v.as_str().unwrap_or_default());
        }
    }
    let res = req.send(&f.data[..]).map_err(|e| ReportError {
        code: "network".into(),
        message: format!("upload of {}: {e}", f.name),
        hint: String::new(),
    })?;
    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(ReportError {
            code: "upload_failed".into(),
            message: format!("storage answered {status} for {}", f.name),
            hint: "start the file again (within an hour of the report)".into(),
        });
    }
    Ok(start["attachment"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_has_kind_note_set_build_fields_and_meta() {
        let b = body(&Report {
            kind: Kind::Crash,
            note: "boom".into(),
            build: Build {
                version: Some("1.0".into()),
                os: Some("macOS 15".into()),
                ..Default::default()
            },
            meta: json!({ "frame": 9 }),
            files: vec![],
        });
        assert_eq!(
            b,
            json!({ "kind": "crash", "note": "boom", "build": { "version": "1.0", "os": "macOS 15" }, "meta": { "frame": 9 } })
        );
        assert!(body(&Report::default()).get("meta").is_none());
    }

    #[test]
    fn sha256_is_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn api_errors_keep_their_code() {
        let e = ok(json!({ "ok": false, "error": { "code": "rate_limited", "message": "at most 10", "hint": "" } })).unwrap_err();
        assert_eq!(e.code, "rate_limited");
        assert_eq!(e.to_string(), "rate_limited: at most 10");
    }
}
