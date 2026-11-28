//! Slice G4e-3: the Authority's listener for people (spec §7.6.4), against a fake identity
//! provider. A person signs in through the tenant, acts under the session, and every
//! refusal of a sign-in looks the same.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use gov_authority::admin_http::{router, Admin, AdminConfig};
use gov_authority::{Account, Authority, MemoryStore, Role};
use gov_core::admin::{AdminAction, AdminRecord};
use gov_core::{Id, Scope, SigningKey};
use gov_signin::testing::FakeIdp;
use gov_signin::{Tenant, Verifier};
use serde_json::{json, Value};

const TENANT: &str = "72f988bf-0000-4000-8000-0000000000aa";
const CLIENT: &str = "0b7f0c6e-1111-4222-8333-444455556666";
const SECRET: &str = "the-client-secret";
const ANA: &str = "00000000-0000-0000-0000-00000000a0a0";
const BEN: &str = "00000000-0000-0000-0000-00000000b0b0";

/// Seconds added to the listener's clock.
static LATER: AtomicI64 = AtomicI64::new(0);
fn clock() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    now + LATER.load(Ordering::SeqCst)
}

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn key(kid: &str, seed: u8) -> SigningKey {
    SigningKey::from_seed(id(kid), &[seed; 32]).unwrap()
}

fn config(origin: &str) -> AdminConfig {
    AdminConfig {
        origin: origin.into(),
        client_secret: SECRET.into(),
        client_header: None,
        sign_ins_per_second: 1000,
        sign_ins_per_client_per_second: 1000,
        fake_data: true,
    }
}

struct Site {
    idp: FakeIdp,
    authority: Arc<Authority<MemoryStore>>,
    admin: Arc<Admin<MemoryStore>>,
    origin: String,
    http: reqwest::Client,
}

async fn site() -> Site {
    let idp = FakeIdp::start(TENANT).await;
    idp.set_client(CLIENT, SECRET);
    let authority = Authority::new(
        MemoryStore::default(),
        Scope::new("acme", "prod").unwrap(),
        id("authority"),
        key("authority-admin-1", 60),
        key("authority-root-1", 61).public(),
    )
    .with_online_keys(
        key("authority:mandates-1", 71),
        key("authority:leases-1", 72),
    );
    let (ana, ben) = (id("ana"), id("ben"));
    let now = clock();
    authority.bootstrap(&ana, now).await.unwrap();
    authority
        .add_person(&ana, &ben, Role::Owner, now)
        .await
        .unwrap();
    for (person, oid) in [(&ana, ANA), (&ben, BEN)] {
        let account = Account {
            tid: TENANT.into(),
            oid: oid.into(),
        };
        authority
            .link_person(&ana, person, &account, now)
            .await
            .unwrap();
    }
    let authority = Arc::new(authority);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let tenant = Tenant::from_settings(["T", "C", "A"], TENANT, CLIENT, Some(&idp.base)).unwrap();
    let admin = Arc::new(
        Admin::new(
            authority.clone(),
            Verifier::new(tenant).unwrap(),
            config(&origin),
            clock,
        )
        .unwrap(),
    );
    let app = router(admin.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Site {
        idp,
        authority,
        admin,
        origin,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    }
}

/// What a browser holds after a sign-in.
struct Browser {
    session: String,
    csrf: String,
}

fn cookie_of(answer: &reqwest::Response, name: &str) -> Option<String> {
    answer
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
        .map(|v| v.split(';').next().unwrap().to_string())
}

impl Site {
    /// The start of a sign-in: the pre-sign-in cookie and where the browser is sent.
    async fn start(&self, query: &str) -> (String, String) {
        let begun = self
            .http
            .get(format!("{}/v1/signin/start{query}", self.origin))
            .send()
            .await
            .unwrap();
        assert_eq!(begun.status(), 302);
        let pre = cookie_of(&begun, "gov_pre").unwrap();
        assert!(begun.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("SameSite=Lax"));
        (
            pre,
            begun.headers()["location"].to_str().unwrap().to_string(),
        )
    }

    /// The tenant's answer: where it sends the browser back to.
    async fn at_the_tenant(&self, to: &str) -> String {
        let back = self.http.get(to).send().await.unwrap();
        assert_eq!(back.status(), 302);
        back.headers()["location"].to_str().unwrap().to_string()
    }

    async fn sign_in(&self, oid: &str) -> Browser {
        self.idp.set_claim("oid", json!(oid));
        let (pre, to) = self.start("").await;
        let back = self.at_the_tenant(&to).await;
        let landed = self
            .http
            .get(back)
            .header("cookie", pre)
            .send()
            .await
            .unwrap();
        assert_eq!(landed.status(), 200);
        let headers = landed.headers();
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["referrer-policy"], "no-referrer");
        let session = cookie_of(&landed, "gov_session").unwrap();
        let kept = headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .find(|v| v.starts_with("gov_session="))
            .unwrap();
        assert!(kept.contains("HttpOnly") && kept.contains("SameSite=Strict"));
        let body: Value = landed.json().await.unwrap();
        Browser {
            session,
            csrf: body["csrf"].as_str().unwrap().to_string(),
        }
    }

    fn post(&self, browser: &Browser, path: &str, body: Value) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.origin))
            .header("cookie", &browser.session)
            .header("origin", &self.origin)
            .header("x-gov-csrf", &browser.csrf)
            .json(&body)
    }

    async fn records(&self) -> Vec<AdminRecord> {
        self.authority.records().await.unwrap()
    }
}

#[tokio::test]
async fn a_person_signs_in_and_acts_under_the_session() {
    let site = site().await;
    let ana = site.sign_in(ANA).await;
    // Only `openid` is asked for, by code with PKCE, and no fresh sign-in unless asked.
    let asked = site.idp.asked();
    assert_eq!(
        (
            asked["scope"].as_str(),
            asked["response_type"].as_str(),
            asked["response_mode"].as_str(),
            asked["code_challenge_method"].as_str(),
            asked["max_age"].as_str(),
        ),
        // A tenant sign-in no older than a session here may last.
        ("openid", "code", "query", "S256", "28800")
    );
    let me: Value = site
        .http
        .get(format!("{}/v1/me", site.origin))
        .header("cookie", &ana.session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (&me["person"], &me["role"]),
        (&json!("ana"), &json!("admin"))
    );

    // An act under the session is recorded as that person's, verified.
    let made = site.post(&ana, "/v1/gates", json!({"id": "gate-1"}));
    assert_eq!(made.send().await.unwrap().status(), 200);
    let last = site.records().await.pop().unwrap();
    assert_eq!(
        (last.action, last.actor.id.as_str(), last.actor.verified),
        (AdminAction::GateRegistered, "ana", true)
    );
    assert_eq!(
        last.actor.session.map(|s| json!(s)),
        Some(me["session"].clone())
    );

    // A request that changes something needs this origin, a JSON body and the session's
    // second value; a request for another host is not this listener's.
    let before = site.records().await.len();
    let gate = json!({"id": "gate-2"});
    let from_elsewhere = site
        .post(&ana, "/v1/gates", gate.clone())
        .header("origin", "https://evil.example");
    assert_eq!(from_elsewhere.send().await.unwrap().status(), 403);
    let no_origin = site
        .http
        .post(format!("{}/v1/gates", site.origin))
        .header("cookie", &ana.session)
        .header("x-gov-csrf", &ana.csrf)
        .json(&gate);
    assert_eq!(no_origin.send().await.unwrap().status(), 403);
    let wrong_value = site
        .post(&ana, "/v1/gates", gate.clone())
        .header("x-gov-csrf", "another");
    assert_eq!(wrong_value.send().await.unwrap().status(), 403);
    // The same with one header each, wrong: not a matter of which of two is read.
    let alone = |origin: &str, csrf: &str, session: &str| {
        site.http
            .post(format!("{}/v1/gates", site.origin))
            .header("cookie", session)
            .header("origin", origin)
            .header("x-gov-csrf", csrf)
            .json(&gate)
    };
    for (origin, csrf, session, want) in [
        (
            "https://evil.example",
            ana.csrf.as_str(),
            ana.session.as_str(),
            403,
        ),
        (site.origin.as_str(), "another", ana.session.as_str(), 403),
        (
            site.origin.as_str(),
            ana.csrf.as_str(),
            "gov_session=nothing",
            401,
        ),
    ] {
        let answer = alone(origin, csrf, session).send().await.unwrap();
        assert_eq!(answer.status(), want);
    }
    let as_a_form = site
        .post(&ana, "/v1/gates", gate.clone())
        .header("content-type", "text/plain");
    assert_eq!(as_a_form.send().await.unwrap().status(), 415);
    let no_session = site
        .post(&ana, "/v1/gates", gate.clone())
        .header("cookie", "gov_session=nothing");
    assert_eq!(no_session.send().await.unwrap().status(), 401);
    let other_host = site
        .http
        .get(format!("{}/v1/me", site.origin))
        .header("host", "evil.example")
        .header("cookie", &ana.session);
    assert_eq!(other_host.send().await.unwrap().status(), 403);
    assert_eq!(site.records().await.len(), before);

    // The library's rules decide: an owner registers no gate.
    let ben = site.sign_in(BEN).await;
    let refused = site.post(&ben, "/v1/gates", gate.clone());
    assert_eq!(refused.send().await.unwrap().status(), 403);
    let unknown = site.post(&ana, "/v1/people/nobody/offboard", json!({}));
    assert_eq!(unknown.send().await.unwrap().status(), 404);
    let twice = site.post(&ana, "/v1/people/ben/link", json!({"oid": ANA}));
    assert_eq!(twice.send().await.unwrap().status(), 409);

    // What changes who is trusted needs a recent sign-in, and says so.
    LATER.store(400, Ordering::SeqCst);
    let old = site.post(&ana, "/v1/gates/gate-1/disable", json!({}));
    let old = old.send().await.unwrap();
    assert_eq!(old.status(), 401);
    assert_eq!(
        old.json::<Value>().await.unwrap(),
        json!({"sign_in_again": true})
    );
    let (_, to) = site.start("?recent=true").await;
    assert!(to.contains("max_age=0"));
    LATER.store(0, Ordering::SeqCst);

    // Signed out, the session is gone.
    let out = site.post(&ana, "/v1/signout", json!({}));
    assert_eq!(out.send().await.unwrap().status(), 200);
    let after = site
        .http
        .get(format!("{}/v1/me", site.origin))
        .header("cookie", &ana.session);
    assert_eq!(after.send().await.unwrap().status(), 401);
}

#[tokio::test]
async fn every_refused_sign_in_looks_the_same() {
    let site = site().await;
    let landing = |back: String, pre: Option<String>| {
        let mut request = site.http.get(back);
        if let Some(pre) = pre {
            request = request.header("cookie", pre);
        }
        async move {
            let answer = request.send().await.unwrap();
            let status = answer.status().as_u16();
            let session = cookie_of(&answer, "gov_session");
            (status, session, answer.text().await.unwrap())
        }
    };
    let refused = (403, None, String::new());
    site.idp.set_claim("oid", json!(ANA));

    // No pre-sign-in cookie: a sign-in someone else started.
    let (_, to) = site.start("").await;
    let back = site.at_the_tenant(&to).await;
    assert_eq!(landing(back, None).await, refused);
    // Another flow's cookie, and a state that is not this flow's.
    let (pre, to) = site.start("").await;
    let (other, _) = site.start("").await;
    let back = site.at_the_tenant(&to).await;
    assert_eq!(landing(back.clone(), Some(other)).await, refused);
    let wrong_state = format!("{}&state=another", back.split("&state=").next().unwrap());
    assert_eq!(landing(wrong_state, Some(pre.clone())).await, refused);
    // That used the flow up: the right address no longer works either.
    assert_eq!(landing(back, Some(pre)).await, refused);
    // A flow, and with it its code, is used once.
    let (pre, to) = site.start("").await;
    let back = site.at_the_tenant(&to).await;
    assert_eq!(landing(back.clone(), Some(pre.clone())).await.0, 200);
    assert_eq!(landing(back, Some(pre)).await, refused);

    // An account linked to nobody, a guest, and a token without the claims that tell.
    for (claim, value) in [
        ("oid", json!("00000000-0000-0000-0000-00000000c0c0")),
        ("acct", json!(1)),
        ("auth_time", Value::Null),
        ("tid", json!("72f988bf-0000-4000-8000-0000000000bb")),
    ] {
        site.idp.set_claim(claim, value);
        let (pre, to) = site.start("").await;
        let back = site.at_the_tenant(&to).await;
        assert_eq!(landing(back, Some(pre)).await, refused, "{claim}");
        site.idp.set_claim("oid", json!(ANA));
        site.idp.set_claim("acct", json!(0));
        site.idp.set_claim("auth_time", json!(clock()));
        site.idp.set_claim("tid", json!(TENANT));
    }
    // A tenant sign-in older than a session here may last, counted under its own reason.
    site.idp.set_claim("auth_time", json!(clock() - 9 * 3600));
    let (pre, to) = site.start("").await;
    let back = site.at_the_tenant(&to).await;
    assert_eq!(landing(back, Some(pre)).await, refused);
    site.idp.set_claim("auth_time", json!(clock()));
    // The operator sees why; the caller did not.
    let why = site.admin.counters.failed.lock().unwrap().clone();
    for reason in [
        "signed_in_too_long_ago",
        "no_flow",
        "state",
        "not_linked",
        "not_a_member",
        "entra_misconfigured",
    ] {
        assert!(why.contains_key(reason), "{reason}: {why:?}");
    }
}

#[tokio::test]
async fn the_listener_starts_only_where_it_is_safe_to() {
    let idp = FakeIdp::start(TENANT).await;
    let authority = || {
        Arc::new(Authority::new(
            MemoryStore::default(),
            Scope::new("acme", "prod").unwrap(),
            id("authority"),
            key("authority-admin-1", 60),
            key("authority-root-1", 61).public(),
        ))
    };
    let verifier = |authority: Option<&str>| {
        Verifier::new(Tenant::from_settings(["T", "C", "A"], TENANT, CLIENT, authority).unwrap())
            .unwrap()
    };
    let started = |origin: &str, change: fn(&mut AdminConfig), idp: Option<&str>| {
        let mut config = config(origin);
        change(&mut config);
        Admin::new(authority(), verifier(idp), config, clock).is_ok()
    };
    let none: fn(&mut AdminConfig) = |_| {};
    let real: fn(&mut AdminConfig) = |c| c.fake_data = false;
    let behind_a_proxy: fn(&mut AdminConfig) = |c| {
        c.fake_data = false;
        c.client_header = Some("x-forwarded-for".into());
    };
    // Plain HTTP only on a literal loopback address, only with fake data.
    assert!(started("http://127.0.0.1:8795", none, Some(&idp.base)));
    assert!(!started("http://127.0.0.1:8795", real, None));
    assert!(!started("http://localhost:8795", none, Some(&idp.base)));
    assert!(!started("http://gov.example", none, Some(&idp.base)));
    assert!(!started("https://gov.example/admin", behind_a_proxy, None));
    // Beyond loopback the proxy's header must be named.
    assert!(!started("https://gov.example", real, None));
    assert!(started("https://gov.example", behind_a_proxy, None));
    // A plain identity provider is for fake data.
    assert!(!started(
        "https://gov.example",
        behind_a_proxy,
        Some(&idp.base)
    ));
}

#[tokio::test]
async fn sign_ins_are_limited_overall_and_for_each_client() {
    let idp = FakeIdp::start(TENANT).await;
    idp.set_client(CLIENT, SECRET);
    let authority = Arc::new(Authority::new(
        MemoryStore::default(),
        Scope::new("acme", "prod").unwrap(),
        id("authority"),
        key("authority-admin-1", 60),
        key("authority-root-1", 61).public(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let tenant = Tenant::from_settings(["T", "C", "A"], TENANT, CLIENT, Some(&idp.base)).unwrap();
    let mut limits = config(&origin);
    limits.client_header = Some("x-client".into());
    limits.sign_ins_per_second = 12;
    limits.sign_ins_per_client_per_second = 3;
    static AT: AtomicI64 = AtomicI64::new(1_800_000_000);
    let admin = Arc::new(
        Admin::new(authority, Verifier::new(tenant).unwrap(), limits, || {
            AT.load(Ordering::SeqCst)
        })
        .unwrap(),
    );
    let app = router(admin);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let start = |client: &'static str| {
        let request = http
            .get(format!("{origin}/v1/signin/start"))
            .header("x-client", client);
        async move { request.send().await.unwrap().status().as_u16() }
    };
    for _ in 0..3 {
        assert_eq!(start("10.0.0.1").await, 302);
    }
    // One client past its own limit is refused, and uses up nobody else's (GR-205).
    for _ in 0..40 {
        assert_eq!(start("10.0.0.1").await, 429);
    }
    // The address is the last one in the header, which the proxy wrote: what a caller
    // puts before it does not make it someone else (GR-206).
    assert_eq!(start("203.0.113.9, 10.0.0.1").await, 429);
    assert_eq!(start("10.0.0.1, 10.0.0.2").await, 302);
    // Not an address, or no header at all: not through the proxy.
    assert_eq!(start("someone").await, 429);
    let bare = http.get(format!("{origin}/v1/signin/start"));
    assert_eq!(bare.send().await.unwrap().status(), 429);
    // Overall: twelve in the second, whoever asks.
    for n in 3..11 {
        assert_eq!(
            start(Box::leak(format!("10.0.0.{n}").into_boxed_str())).await,
            302
        );
    }
    assert_eq!(start("10.0.0.99").await, 429);
    AT.fetch_add(1, Ordering::SeqCst);
    assert_eq!(start("10.0.0.1").await, 302);
}

/// On an https origin both cookies are the host's alone: a sibling host cannot plant the
/// pre-sign-in cookie (GR-207).
#[tokio::test]
async fn on_https_the_cookies_are_the_hosts_alone() {
    let idp = FakeIdp::start(TENANT).await;
    let authority = Arc::new(Authority::new(
        MemoryStore::default(),
        Scope::new("acme", "prod").unwrap(),
        id("authority"),
        key("authority-admin-1", 60),
        key("authority-root-1", 61).public(),
    ));
    let tenant = Tenant::from_settings(["T", "C", "A"], TENANT, CLIENT, Some(&idp.base)).unwrap();
    let mut behind = config("https://gov.example");
    behind.client_header = Some("x-client".into());
    let admin =
        Arc::new(Admin::new(authority, Verifier::new(tenant).unwrap(), behind, clock).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap();
    let app = router(admin);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let begun = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("http://{at}/v1/signin/start"))
        .header("host", "gov.example")
        .header("x-client", "10.0.0.1")
        .send()
        .await
        .unwrap();
    assert_eq!(begun.status(), 302);
    let kept = begun.headers()["set-cookie"].to_str().unwrap();
    assert!(kept.starts_with("__Host-gov_pre="), "{kept}");
    for part in ["Secure", "HttpOnly", "SameSite=Lax", "Path=/;"] {
        assert!(kept.contains(part), "{part}: {kept}");
    }
    let to = begun.headers()["location"].to_str().unwrap();
    assert!(to.contains("redirect_uri=https%3A%2F%2Fgov.example%2Fv1%2Fsignin%2Fcallback"));
}

/// Every route that changes something acts as the session, never as a bare name: its
/// record is verified and carries the session (review finding GR-204).
#[tokio::test]
async fn every_changing_route_records_a_verified_act() {
    use gov_core::enrol::{code_digest, EnrolKind, EnrolRequest};
    use gov_core::policy::{OnlineKeys, PolicyBundle};
    let site = site().await;
    let ana = site.sign_in(ANA).await;
    let me: Value = site
        .http
        .get(format!("{}/v1/me", site.origin))
        .header("cookie", &ana.session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let grants = json!([{
        "system": "jira",
        "operations": ["issue.update"],
        "resources": ["jira/project/pay/*"],
    }]);
    let bundle = PolicyBundle {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        version: id("v"),
        number: 1,
        issued_at: clock(),
        keys: OnlineKeys {
            mandates: vec![key("authority:mandates-1", 71).public()],
            leases: vec![key("authority:leases-1", 72).public()],
        },
        freshness: Default::default(),
        rules: Vec::new(),
        connectors: Vec::new(),
        templates: vec![gov_core::mandate::Template {
            id: id("triage"),
            grants: serde_json::from_value(grants.clone()).unwrap(),
            max_life_secs: 3600,
            limits: Default::default(),
            delegable: false,
            people: Default::default(),
            receivers: Default::default(),
        }],
    };
    let signed = gov_core::seal(&bundle, &key("authority-root-1", 61)).unwrap();

    let act = |path: String, body: Value, want: AdminAction| {
        let request = site.post(&ana, &path, body);
        let (site, session) = (&site, me["session"].clone());
        async move {
            let answer = request.send().await.unwrap();
            assert_eq!(answer.status(), 200, "{path}");
            let body: Value = answer.json().await.unwrap();
            let last = site.records().await.pop().unwrap();
            assert_eq!(
                (last.action, last.actor.verified, json!(last.actor.session)),
                (want, true, session),
                "{path}"
            );
            body
        }
    };
    use AdminAction as A;
    let none = || json!({});
    act(
        "/v1/people".into(),
        json!({"id": "cy", "role": "owner"}),
        A::PersonAdded,
    )
    .await;
    let cys = json!({"oid": "00000000-0000-0000-0000-00000000c0c0"});
    act("/v1/people/cy/link".into(), cys, A::PersonLinked).await;
    let bot = json!({"id": "bot", "owner": "ana"});
    act("/v1/agents".into(), bot, A::AgentRegistered).await;
    act(
        "/v1/gates".into(),
        json!({"id": "gate-1"}),
        A::GateRegistered,
    )
    .await;
    let offer = json!({"kind": "agent", "name": "bot"});
    let offered = act("/v1/enrolments".into(), offer, A::EnrolmentOffered).await;
    // The agent enrols its own key with the code: not a person's act.
    let code = offered["code"].as_str().unwrap();
    let bots = key("bot:k1", 9);
    let asked = gov_core::seal(
        &EnrolRequest {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            authority: id("authority"),
            kind: EnrolKind::Agent,
            name: id("bot"),
            key: bots.public(),
            code: code_digest(code),
        },
        &bots,
    )
    .unwrap();
    site.authority.enrol(code, &asked, clock()).await.unwrap();
    act("/v1/bundles".into(), json!(signed), A::PolicyPublished).await;
    let ask = json!({
        "agent": "bot", "key": "bot:k1", "template": "triage",
        "grants": grants, "life_secs": 600,
    });
    act("/v1/mandates".into(), ask, A::MandateIssued).await;
    let listed: Value = site
        .http
        .get(format!("{}/v1/mandates", site.origin))
        .header("cookie", &ana.session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mandate = listed[0]["id"].as_str().unwrap();
    act(format!("/v1/mandates/{mandate}/revoke"), none(), A::Revoked).await;
    act("/v1/agents/bot/suspend".into(), none(), A::AgentSuspended).await;
    act("/v1/agents/bot/resume".into(), none(), A::AgentResumed).await;
    act("/v1/keys/bot:k1/revoke".into(), none(), A::Revoked).await;
    act("/v1/agents/bot/disable".into(), none(), A::AgentDisabled).await;
    act("/v1/gates/gate-1/disable".into(), none(), A::GateDisabled).await;
    act("/v1/people/cy/offboard".into(), none(), A::PersonOffboarded).await;
    act("/v1/signout".into(), none(), A::SignedOut).await;
}
