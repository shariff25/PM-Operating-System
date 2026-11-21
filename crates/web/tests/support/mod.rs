//! A fake of the tool server's user database routes (`GET /internal/users/resolve`,
//! `POST /internal/users/{handle}/identities/entra`), with the same answers and the same
//! first-link rules as the real one (`crates/toolserver/src/users/link.rs`), for `pmos-web`'s
//! tests. Fake data only.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct FakeUser {
    pub handle: String,
    pub display_name: String,
    pub status: String,
    pub roles: Vec<String>,
    pub sign_in_name: Option<String>,
    pub confirmed: bool,
    /// `(tid, oid, host_confirmed)`.
    pub entra: Option<(String, String, bool)>,
}

impl FakeUser {
    pub fn new(handle: &str, roles: &[&str]) -> Self {
        Self {
            handle: handle.into(),
            display_name: String::new(),
            status: "active".into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            sign_in_name: None,
            confirmed: false,
            entra: None,
        }
    }

    fn host_only(&self) -> bool {
        self.roles.iter().any(|r| r == "admin" || r == "auditor")
    }

    fn entra_state(&self) -> &'static str {
        match &self.entra {
            None => "none",
            Some((_, _, host)) if self.host_only() && !host => "needs_host",
            Some(_) => "linked",
        }
    }

    fn json(&self) -> Value {
        json!({
            "handle": self.handle,
            "display_name": self.display_name,
            "status": self.status,
            "roles": self.roles,
            "entra": self.entra_state(),
            "sign_in_name_confirmed": self.confirmed,
            "dual_role": self.roles.iter().any(|r| r == "admin")
                && self.roles.iter().any(|r| r == "auditor"),
        })
    }
}

#[derive(Default)]
pub struct FakeDirectory {
    users: Mutex<Vec<FakeUser>>,
    down: AtomicBool,
    /// Every `resolve` call answered (not counting refusals while down).
    pub resolves: AtomicUsize,
    /// Handles linked by the first-link route.
    pub links: Mutex<Vec<String>>,
    /// Conflicts the first-link route audited (`users.identity_conflict`), by target handle.
    pub conflicts: Mutex<Vec<String>>,
    /// Every first-link body received (`{id_token, nonce}`).
    pub link_bodies: Mutex<Vec<Value>>,
    /// What `GET /internal/users/entra-link` answers: the tenant and client id first-link
    /// tokens are checked for, or `None` (not configured).
    pub entra_link: Mutex<Option<(String, String)>>,
}

fn coded(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"code": code, "error": code}))).into_response()
}

fn query(uri: &Uri) -> Vec<(String, String)> {
    uri.query().map(url_pairs).unwrap_or_default()
}

fn url_pairs(q: &str) -> Vec<(String, String)> {
    reqwest::Url::parse(&format!("http://x/?{q}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

impl FakeDirectory {
    /// Every member of a `members:` list (the seed's shape) as an active user: the list's
    /// roles, its `email` as a confirmed sign-in name, no Entra link. Read here as plain YAML:
    /// only the tool server's import parses that shape (slice U7).
    pub fn seed_team(&self, yaml: &str) {
        let seed: serde_norway::Value = serde_norway::from_str(yaml).unwrap();
        let mut users = self.users.lock().unwrap();
        users.clear();
        for m in seed["members"].as_sequence().unwrap() {
            let text = |k: &str| m.get(k).and_then(|v| v.as_str()).map(String::from);
            users.push(FakeUser {
                handle: text("handle").unwrap(),
                display_name: String::new(),
                status: "active".into(),
                roles: m["roles"]
                    .as_sequence()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap().to_string())
                    .collect(),
                sign_in_name: text("email").map(|e| e.to_lowercase()),
                confirmed: true,
                entra: None,
            });
        }
    }

    pub fn add(&self, user: FakeUser) {
        let mut users = self.users.lock().unwrap();
        users.retain(|u| u.handle != user.handle);
        users.push(user);
    }

    pub fn update(&self, handle: &str, change: impl FnOnce(&mut FakeUser)) {
        let mut users = self.users.lock().unwrap();
        change(users.iter_mut().find(|u| u.handle == handle).unwrap());
    }

    pub fn remove(&self, handle: &str) {
        self.users.lock().unwrap().retain(|u| u.handle != handle);
    }

    pub fn get(&self, handle: &str) -> Option<FakeUser> {
        self.users
            .lock()
            .unwrap()
            .iter()
            .find(|u| u.handle == handle)
            .cloned()
    }

    /// The tool server can't be reached (answers `503` to every user route).
    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    /// Answers a user database route, or `None` for any other path.
    pub fn handle(&self, method: &Method, uri: &Uri, body: &[u8]) -> Option<Response> {
        let path: Vec<&str> = uri.path().trim_matches('/').split('/').collect();
        let is_users = path.len() >= 2 && path[0] == "internal" && path[1] == "users";
        if !is_users {
            return None;
        }
        if self.down.load(Ordering::SeqCst) {
            return Some(coded(StatusCode::SERVICE_UNAVAILABLE, "unavailable"));
        }
        Some(match (method.as_str(), path.as_slice()) {
            ("GET", ["internal", "users", "resolve"]) => self.resolve(uri),
            ("GET", ["internal", "users", "entra-link"]) => {
                match self.entra_link.lock().unwrap().clone() {
                    Some((t, c)) => {
                        Json(json!({"configured": true, "tenant_id": t, "client_id": c}))
                            .into_response()
                    }
                    None => {
                        Json(json!({"configured": false, "tenant_id": null, "client_id": null}))
                            .into_response()
                    }
                }
            }
            ("POST", ["internal", "users", handle, "identities", "entra"]) => {
                self.link(handle, body)
            }
            _ => coded(StatusCode::NOT_FOUND, "not_found"),
        })
    }

    fn resolve(&self, uri: &Uri) -> Response {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        let q = query(uri);
        let get = |k: &str| {
            q.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let users = self.users.lock().unwrap();
        let found = match get("by").as_str() {
            "handle" => users.iter().find(|u| u.handle == get("handle")),
            "sign_in_name" => {
                let name = get("name").trim().to_lowercase();
                users
                    .iter()
                    .find(|u| u.sign_in_name.as_deref() == Some(name.as_str()))
            }
            "entra" => {
                let (tid, oid) = (get("tid").to_lowercase(), get("oid").to_lowercase());
                users.iter().find(|u| {
                    u.entra
                        .as_ref()
                        .is_some_and(|(t, o, _)| *t == tid && *o == oid)
                })
            }
            _ => return coded(StatusCode::UNPROCESSABLE_ENTITY, "invalid"),
        };
        match found {
            Some(u) => Json(u.json()).into_response(),
            None => coded(StatusCode::NOT_FOUND, "not_found"),
        }
    }

    /// The first link, in the real route's order (`users/link.rs`, `plan`). The body is
    /// `{id_token, nonce}` as the real route takes it; the fake reads the token's claims
    /// without checking its signature (the real tool server does: `crates/toolserver/tests/
    /// entra_link.rs`), and keeps each body for the tests.
    fn link(&self, handle: &str, body: &[u8]) -> Response {
        let b: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        self.link_bodies.lock().unwrap().push(b.clone());
        let claims = b["id_token"]
            .as_str()
            .and_then(|t| t.split('.').nth(1))
            .and_then(|p| {
                base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, p).ok()
            })
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or(Value::Null);
        if b.as_object().map(|o| o.len()) != Some(2)
            || b["nonce"].as_str().is_none()
            || claims["nonce"] != b["nonce"]
        {
            return coded(StatusCode::UNPROCESSABLE_ENTITY, "invalid_token");
        }
        let lower = |v: &Value| v.as_str().map(|s| s.trim().to_lowercase());
        let (Some(tid), Some(oid), Some(name)) = (
            lower(&claims["tid"]),
            lower(&claims["oid"]),
            lower(&claims["preferred_username"]),
        ) else {
            return coded(StatusCode::UNPROCESSABLE_ENTITY, "invalid_token");
        };
        let (tid, oid, name) = (tid.as_str(), oid.as_str(), name.as_str());
        let mut users = self.users.lock().unwrap();
        let Some(i) = users
            .iter()
            .position(|u| u.handle == handle && u.sign_in_name.as_deref() == Some(name))
        else {
            return coded(StatusCode::NOT_FOUND, "not_found");
        };
        let conflict = |code: &str| {
            self.conflicts.lock().unwrap().push(handle.to_string());
            coded(StatusCode::CONFLICT, code)
        };
        match &users[i].entra {
            Some((t, o, _)) if t == tid && o == oid => {
                return Json(json!({"first_link": false, "user": users[i].json()})).into_response()
            }
            Some(_) => return conflict("linked"),
            None => {}
        }
        let taken = users.iter().any(|u| {
            u.handle != handle
                && u.entra
                    .as_ref()
                    .is_some_and(|(t, o, _)| t == tid && o == oid)
        });
        if taken {
            return conflict("taken");
        }
        let u = &mut users[i];
        if u.status != "active" {
            return coded(StatusCode::FORBIDDEN, "not_active");
        }
        if u.host_only() {
            return coded(StatusCode::FORBIDDEN, "host_only");
        }
        if !u.confirmed {
            return coded(StatusCode::FORBIDDEN, "sign_in_unconfirmed");
        }
        u.entra = Some((tid.to_string(), oid.to_string(), false));
        self.links.lock().unwrap().push(handle.to_string());
        Json(json!({"first_link": true, "user": u.json()})).into_response()
    }
}

/// One call the fake bot routes received, as sent.
#[derive(Debug, Clone)]
pub struct BotsCall {
    pub method: String,
    pub uri: String,
    pub body: Vec<u8>,
    pub assertion: Option<String>,
}

/// A team row: `(key, added_at, added_via)`.
pub type TeamRow = (String, String, String);

/// A fake of the tool server's bot marketplace routes (slice B1, `crates/toolserver/src/app/
/// team.rs`): `GET /internal/personas`, `GET` and `PUT /internal/team/{handle}`, with the
/// real routes' answers and refusal codes. Answered apart from the other routes, so the tests
/// that count tool server calls count what they always did. Fake data only.
pub struct FakeBots {
    /// The offered personas, as `GET /internal/personas` lists them.
    pub catalog: Mutex<Vec<Value>>,
    /// Each person's rows.
    pub teams: Mutex<BTreeMap<String, Vec<TeamRow>>>,
    /// `drafts_this_month` by handle.
    pub drafts: Mutex<BTreeMap<String, Value>>,
    /// Handles the team routes answer `403 not_allowed` for (an Auditor-only person).
    pub not_allowed: Mutex<Vec<String>>,
    /// A refusal every team change answers with: `(status, code)`.
    pub refuse_change: Mutex<Option<(u16, String)>>,
    /// The routes answer `503 unavailable`.
    down: AtomicBool,
    pub calls: Mutex<Vec<BotsCall>>,
}

/// The always-on persona as the catalog serves it.
pub fn nudge_persona() -> Value {
    json!({"key": "nudge", "display_name": "Nudge", "title": "Your PM assistant",
           "mascot": "nudge", "always_on": true, "summary": "Fake: does a bit of everything.",
           "examples": ["Fake: draft a PRD"], "can_do": ["ask"], "reads": ["jira"],
           "drafts": ["Jira issues"], "needs": []})
}

impl Default for FakeBots {
    fn default() -> Self {
        Self {
            catalog: Mutex::new(vec![nudge_persona()]),
            teams: Mutex::default(),
            drafts: Mutex::default(),
            not_allowed: Mutex::default(),
            refuse_change: Mutex::default(),
            down: AtomicBool::new(false),
            calls: Mutex::default(),
        }
    }
}

impl FakeBots {
    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    pub fn calls(&self) -> Vec<BotsCall> {
        self.calls.lock().unwrap().clone()
    }

    /// The keys of `handle`'s rows.
    pub fn team_rows(&self, handle: &str) -> Vec<String> {
        self.teams
            .lock()
            .unwrap()
            .get(handle)
            .map(|r| r.iter().map(|(k, ..)| k.clone()).collect())
            .unwrap_or_default()
    }

    fn offered(&self, key: &str) -> bool {
        self.catalog
            .lock()
            .unwrap()
            .iter()
            .any(|p| p["key"] == json!(key))
    }

    fn team(&self, handle: &str) -> Value {
        let catalog = self.catalog.lock().unwrap().clone();
        let rank = |k: &str| catalog.iter().position(|p| p["key"] == json!(k));
        let mut rows = self
            .teams
            .lock()
            .unwrap()
            .get(handle)
            .cloned()
            .unwrap_or_default();
        rows.sort_by(|a, b| {
            let (ra, rb) = (rank(&a.0), rank(&b.0));
            ra.is_none()
                .cmp(&rb.is_none())
                .then(ra.cmp(&rb))
                .then(a.0.cmp(&b.0))
        });
        let mut bots =
            vec![json!({"key": "nudge", "always_on": true, "added_at": null, "offered": true})];
        for (k, at, via) in rows {
            let offered = self.offered(&k);
            bots.push(
                json!({"key": k, "always_on": false, "added_at": at, "added_via": via,
                             "offered": offered}),
            );
        }
        let drafts = self
            .drafts
            .lock()
            .unwrap()
            .get(handle)
            .cloned()
            .unwrap_or_else(|| json!({}));
        json!({"handle": handle, "bots": bots, "drafts_this_month": drafts})
    }

    /// Answers a bot marketplace route, or `None` for any other path.
    pub fn handle(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Option<Response> {
        let path: Vec<&str> = uri.path().trim_matches('/').split('/').collect();
        let ours = matches!(
            path.as_slice(),
            ["internal", "personas"] | ["internal", "team", _]
        );
        if !ours {
            return None;
        }
        self.calls.lock().unwrap().push(BotsCall {
            method: method.to_string(),
            uri: uri.to_string(),
            body: body.to_vec(),
            assertion: headers
                .get("x-pmos-assertion")
                .and_then(|v| v.to_str().ok())
                .map(String::from),
        });
        if self.down.load(Ordering::SeqCst) {
            return Some(coded(StatusCode::SERVICE_UNAVAILABLE, "unavailable"));
        }
        let handle = match path.as_slice() {
            ["internal", "personas"] => {
                return Some(
                    Json(json!({
                        "catalog_sha": "0123abcd",
                        "personas": self.catalog.lock().unwrap().clone(),
                    }))
                    .into_response(),
                )
            }
            [_, _, handle] => handle.to_string(),
            _ => return Some(coded(StatusCode::NOT_FOUND, "not_found")),
        };
        if self.not_allowed.lock().unwrap().contains(&handle) {
            return Some(coded(StatusCode::FORBIDDEN, "not_allowed"));
        }
        if method == Method::GET {
            return Some(Json(self.team(&handle)).into_response());
        }
        if method != Method::PUT {
            return Some(coded(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"));
        }
        if let Some((status, code)) = self.refuse_change.lock().unwrap().clone() {
            return Some(coded(StatusCode::from_u16(status).unwrap(), &code));
        }
        Some(self.change(&handle, body))
    }

    fn change(&self, handle: &str, body: &[u8]) -> Response {
        let b: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let keys = |k: &str| -> Vec<String> {
            b[k].as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        let (add, remove) = (keys("add"), keys("remove"));
        let shaped = b.as_object().is_some_and(|o| {
            o.keys()
                .all(|k| matches!(k.as_str(), "add" | "remove" | "via"))
        }) && matches!(
            b["via"].as_str(),
            Some("onboarding" | "bots_page" | "picker")
        );
        if !shaped || (add.is_empty() && remove.is_empty()) {
            return coded(StatusCode::BAD_REQUEST, "bad_request");
        }
        if add.iter().chain(&remove).any(|k| k == "nudge") {
            return coded(StatusCode::CONFLICT, "always_on");
        }
        if add.iter().any(|k| !self.offered(k)) {
            return coded(StatusCode::CONFLICT, "bot_unavailable");
        }
        {
            let mut teams = self.teams.lock().unwrap();
            let rows = teams.entry(handle.to_string()).or_default();
            rows.retain(|(k, ..)| !remove.contains(k));
            for k in add {
                if !rows.iter().any(|(r, ..)| *r == k) {
                    let via = b["via"].as_str().unwrap_or_default().to_string();
                    rows.push((k, "2026-11-20T09:01:00Z".into(), via));
                }
            }
        }
        Json(self.team(handle)).into_response()
    }
}
