//! A fake of the tool server's user database routes (`GET /internal/users/resolve`,
//! `POST /internal/users/{handle}/identities/entra`), with the same answers and the same
//! first-link rules as the real one (`crates/toolserver/src/users/link.rs`), for `pmos-web`'s
//! tests. Fake data only.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use axum::http::{Method, StatusCode, Uri};
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

    /// The first link, in the real route's order (`users/link.rs`, `plan`).
    fn link(&self, handle: &str, body: &[u8]) -> Response {
        let b: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let (Some(tid), Some(oid), Some(name)) = (
            b["tid"].as_str(),
            b["oid"].as_str(),
            b["sign_in_name"].as_str(),
        ) else {
            return coded(StatusCode::UNPROCESSABLE_ENTITY, "invalid");
        };
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
