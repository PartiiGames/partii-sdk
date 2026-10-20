//! The partii API client. [`OPS`] (generated from partii's public OpenAPI
//! spec, <https://partii.games/docs/openapi.yaml>) lists every operation;
//! [`Client::call`] runs one, and [`Client::json`] is the short form for
//! code that isn't a CLI. The `partii` CLI builds a command per operation from
//! the same table, so the API, the client and the CLI can't drift apart.

mod generated;
pub use generated::OPS;

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    Public,
    Key,
    Player,
    Developer,
    /// The moderator's token: player content review only.
    Moderator,
    Stripe,
    /// partii's own operations tooling (not for games): `Client::operator_headers`.
    Operator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loc {
    Path,
    Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    None,
    Json,
    Binary,
}

#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub loc: Loc,
    pub required: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub ty: &'static str,
    pub required: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Op {
    pub id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub auth: Auth,
    pub cli: Option<&'static str>,
    pub summary: &'static str,
    pub params: &'static [Param],
    pub body: Body,
    pub fields: &'static [Field],
    pub binary_response: bool,
}

pub fn op(id: &str) -> Option<&'static Op> {
    OPS.iter().find(|o| o.id == id)
}

/// Credentials and endpoint; set what the operations you call need.
#[derive(Debug, Clone, Default)]
pub struct Client {
    pub base: String,
    pub developer_token: Option<String>,
    pub moderator_token: Option<String>,
    pub key: Option<String>,
    pub player_token: Option<String>,
    /// For operations calls: the operator credential (and Access service token) headers partii-ops made.
    pub operator_headers: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum Reply {
    Json { status: u16, body: Value },
    Bytes { status: u16, bytes: Vec<u8> },
}

#[derive(Debug)]
pub struct CallError {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Client {
    /// Run `op` with path/query `args`, an optional JSON body or raw bytes.
    pub fn call(
        &self,
        op: &Op,
        args: &BTreeMap<String, String>,
        body: Option<&Value>,
        raw: Option<Vec<u8>>,
        dry_run: bool,
    ) -> Result<Reply, CallError> {
        let mut path = op.path.to_string();
        let mut query = Vec::new();
        for p in op.params {
            match (args.get(p.name), p.loc) {
                (Some(v), Loc::Path) => path = path.replace(&format!("{{{}}}", p.name), &encode(v)),
                (Some(v), Loc::Query) => query.push(format!("{}={}", p.name, encode(v))),
                (None, _) if p.required => {
                    return Err(CallError {
                        code: "usage",
                        message: format!("{} needs {}", op.id, p.name),
                        hint: format!("pass {}", p.name),
                    });
                }
                _ => {}
            }
        }
        let mut url = format!("{}{}", self.base.trim_end_matches('/'), path);
        if !query.is_empty() {
            url = format!("{url}?{}", query.join("&"));
        }
        let missing = |what: &str, env: &str| CallError {
            code: "unauthorized",
            message: format!("{} needs a {what}", op.id),
            hint: format!("set {env}"),
        };
        let mut headers: Vec<(&str, String)> = Vec::new();
        match op.auth {
            Auth::Developer => headers.push((
                "authorization",
                format!(
                    "Bearer {}",
                    self.developer_token
                        .as_ref()
                        .ok_or_else(|| missing("developer token", "PARTII_TOKEN"))?
                ),
            )),
            Auth::Moderator => headers.push((
                "authorization",
                format!(
                    "Bearer {}",
                    self.moderator_token
                        .as_ref()
                        .ok_or_else(|| missing("moderator token", "PARTII_MODERATOR_TOKEN"))?
                ),
            )),
            Auth::Operator => {
                if self.operator_headers.is_empty() {
                    return Err(missing(
                        "operator credential",
                        "up ~/.config/partii/ops.env (partii-ops)",
                    ));
                }
                headers.extend(
                    self.operator_headers
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.clone())),
                );
            }
            Auth::Key => headers.push((
                "x-partii-key",
                self.key
                    .clone()
                    .ok_or_else(|| missing("publishable key", "PARTII_KEY"))?,
            )),
            Auth::Player => {
                headers.push((
                    "x-partii-key",
                    self.key
                        .clone()
                        .ok_or_else(|| missing("publishable key", "PARTII_KEY"))?,
                ));
                headers.push((
                    "authorization",
                    format!(
                        "Bearer {}",
                        self.player_token
                            .as_ref()
                            .ok_or_else(|| missing("player token", "PARTII_PLAYER_TOKEN"))?
                    ),
                ));
            }
            Auth::Public | Auth::Stripe => {}
        }
        if let (Auth::Key, Some(t)) = (op.auth, &self.player_token) {
            // Logins may carry the guest being upgraded or merged.
            headers.push(("authorization", format!("Bearer {t}")));
        }
        if dry_run {
            headers.push(("x-partii-dry-run", "1".into()));
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let net = |e: ureq::Error| CallError {
            code: "network",
            message: format!("{url}: {e}"),
            hint: "is PARTII_API right and the platform up? (`partii status`)".into(),
        };
        let mut res = match (op.method, body, raw) {
            ("GET", _, _) => {
                let mut r = agent.get(&url);
                for (k, v) in &headers {
                    r = r.header(*k, v);
                }
                r.call().map_err(net)?
            }
            ("DELETE", _, _) => {
                let mut r = agent.delete(&url);
                for (k, v) in &headers {
                    r = r.header(*k, v);
                }
                r.call().map_err(net)?
            }
            (m, b, r) => {
                let mut req = match m {
                    "POST" => agent.post(&url),
                    "PUT" => agent.put(&url),
                    "PATCH" => agent.patch(&url),
                    other => {
                        return Err(CallError {
                            code: "usage",
                            message: format!("unsupported method {other}"),
                            hint: String::new(),
                        })
                    }
                };
                for (k, v) in &headers {
                    req = req.header(*k, v);
                }
                if let Some(bytes) = r {
                    req.header("content-type", "application/octet-stream")
                        .send(&bytes[..])
                        .map_err(net)?
                } else {
                    let v = b
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Default::default()));
                    req.send_json(&v).map_err(net)?
                }
            }
        };
        let status = res.status().as_u16();
        let is_json = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.contains("json"));
        let mut bytes = Vec::new();
        res.body_mut()
            .as_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| CallError {
                code: "network",
                message: e.to_string(),
                hint: String::new(),
            })?;
        if is_json {
            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            Ok(Reply::Json { status, body })
        } else {
            Ok(Reply::Bytes { status, bytes })
        }
    }

    /// Call by operation id with a JSON body; the reply's JSON (for code that
    /// is not the CLI: tests, tools, games).
    pub fn json(
        &self,
        id: &str,
        args: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value, CallError> {
        let op = op(id).ok_or_else(|| CallError {
            code: "usage",
            message: format!("no operation {id}"),
            hint: String::new(),
        })?;
        let args: BTreeMap<String, String> = args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        match self.call(op, &args, body.as_ref(), None, false)? {
            Reply::Json { body, .. } => Ok(body),
            Reply::Bytes { status, .. } => Err(CallError {
                code: "unexpected",
                message: format!("{id} returned bytes (status {status})"),
                hint: String::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_is_well_formed() {
        assert!(OPS.len() > 40);
        let mut ids = std::collections::BTreeSet::new();
        let mut clis = std::collections::BTreeSet::new();
        for o in OPS {
            assert!(ids.insert(o.id), "duplicate {}", o.id);
            if let Some(c) = o.cli {
                assert!(clis.insert(c), "duplicate CLI command {c}");
            }
            for p in o.params.iter().filter(|p| p.loc == Loc::Path) {
                assert!(
                    o.path.contains(&format!("{{{}}}", p.name)),
                    "{}: {} not in path",
                    o.id,
                    p.name
                );
            }
        }
    }

    #[test]
    fn missing_path_params_and_credentials_are_errors_before_any_request() {
        let c = Client {
            base: "http://127.0.0.1:1".into(),
            ..Default::default()
        };
        let e = c
            .call(op("gamesGet").unwrap(), &BTreeMap::new(), None, None, false)
            .unwrap_err();
        assert_eq!(e.code, "usage");
        let mut args = BTreeMap::new();
        args.insert("game".into(), "tankii".into());
        let e = c
            .call(op("gamesGet").unwrap(), &args, None, None, false)
            .unwrap_err();
        assert_eq!(e.code, "unauthorized");
        assert!(e.hint.contains("PARTII_TOKEN"));
    }

    #[test]
    fn path_params_are_encoded() {
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
    }
}
