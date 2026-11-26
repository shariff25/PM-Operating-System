//! The two programs, run as an operator runs them: keys made on the command line, a gate
//! enrolled, calls made through the gate to a small fake MCP server, and the Ledger's export
//! checked against the witness the way an auditor's `gov verify` checks it.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::time::Duration;

use axum::body::Bytes;
use axum::routing::post;
use axum::{Json, Router};
use gov_core::action::{Kind, Reversal};
use gov_core::connector::{tool_digest, ResourceTemplate, ServerMap, ToolMap};
use gov_core::{Id, PublicKey, Scope};
use gov_ledger::verify::{verify, Check, Report};
use gov_ledger::{DirWitness, Export, TrustedKeys, Witness};
use serde_json::{json, Value};
use sqlx::postgres::PgConnectOptions;
use sqlx::{ConnectOptions, Connection};

const LEDGER: &str = env!("CARGO_BIN_EXE_gov-ledger");
const GATE: &str = env!("CARGO_BIN_EXE_gov-gate");
const SECRET: &str = "Bearer agent-jira-token-do-not-record";

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}

/// A running program, stopped when the test is done with it.
struct Running {
    child: Child,
    addr: String,
    /// Kept open: a program that prints again must still have somewhere to print.
    out: BufReader<std::process::ChildStdout>,
}
impl Running {
    /// The next line the program printed, after `prefix`.
    fn said(&mut self, prefix: &str) -> String {
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        line.trim()
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("the program said {line:?}"))
            .to_string()
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start a program and read where it says it listens.
fn start(mut command: Command) -> Running {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut running = Running {
        child,
        addr: String::new(),
        out,
    };
    running.addr = running.said("listening on ");
    running
}

/// A program, told that the data is fake: keys in files are allowed for nothing else.
fn program(path: &str) -> Command {
    let mut c = Command::new(path);
    c.env("GOV_DATA_CLASS", "fake");
    c
}

/// Run a command to its end and return what it printed.
fn run(command: &mut Command) -> (bool, String, String) {
    let out = command.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn update_tool() -> Value {
    json!({
        "name": "jira_update_issue",
        "description": "Update an issue",
        "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}}},
    })
}

async fn fake_jira(body: Bytes) -> Json<Value> {
    let msg: Value = serde_json::from_slice(&body).unwrap();
    let result = match msg["method"].as_str().unwrap_or_default() {
        "tools/list" => json!({"tools": [update_tool()]}),
        "tools/call" => json!({"content": [{"type": "text", "text": "done: customer 4411"}]}),
        _ => json!({}),
    };
    Json(json!({"jsonrpc": "2.0", "id": msg["id"], "result": result}))
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

/// A fresh schema in Agent Governance's test database, and the address that selects it.
struct Database {
    base: PgConnectOptions,
    schema: String,
    url: String,
}
impl Database {
    async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("GOV_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "GOV_TEST_DATABASE_URL must be set in CI so the programs' test runs"
            );
            eprintln!("skipping: GOV_TEST_DATABASE_URL is not set");
            return None;
        };
        let base = PgConnectOptions::from_str(&url).unwrap();
        let schema = format!("g_{}", uuid::Uuid::new_v4().simple());
        let mut admin = base.connect().await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .unwrap();
        admin.close().await.ok();
        let url = format!("{url}?options=-c%20search_path%3D{schema}");
        Some(Database { base, schema, url })
    }
    async fn drop_schema(self) {
        let mut admin = self.base.connect().await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&mut admin)
        .await
        .ok();
    }
}

struct Site {
    dir: tempfile::TempDir,
    db: Database,
    upstream: SocketAddr,
}
impl Site {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn ledger(&self) -> Command {
        let mut c = program(LEDGER);
        c.env("GOV_LEDGER_DATABASE_URL", &self.db.url);
        c
    }
    fn gate(&self, ledger: &str) -> Command {
        let mut c = program(GATE);
        c.args([
            "serve", "--tenant", "acme", "--env", "prod", "--gate", "gate-1",
        ])
        .args(["--listen", "127.0.0.1:0", "--ship-interval", "1"])
        .args(["--operator-listen", "127.0.0.1:0"])
        .arg("--key-file")
        .arg(self.path("gate.key"))
        .arg("--spool")
        .arg(self.path("spool"))
        .arg("--map")
        .arg(self.path("map.json"))
        .arg("--ledger-keys")
        .arg(self.path("ledger-keys.json"))
        .arg("--witness")
        .arg(self.path("witness"))
        .args(["--ledger", ledger])
        .args(["--upstream", &format!("http://{}/mcp", self.upstream)]);
        c
    }
    /// What an auditor does: export, then check the export against the witness.
    fn audit(&self) -> Report {
        let (ok, said, err) = run(self
            .ledger()
            .args(["export", "--tenant", "acme", "--env", "prod", "--key-file"])
            .arg(self.path("ledger.key"))
            .arg("--out")
            .arg(self.path("export.json"))
            .arg("--keys-out")
            .arg(self.path("keys.json")));
        assert!(ok, "{err}");
        // The keys file comes from the Ledger being audited, and the command says so.
        assert!(said.contains("not evidence"), "{said}");
        let export: Export =
            serde_json::from_slice(&std::fs::read(self.path("export.json")).unwrap()).unwrap();
        let keys: TrustedKeys =
            serde_json::from_slice(&std::fs::read(self.path("keys.json")).unwrap()).unwrap();
        verify(&Check {
            scope: &Scope::new("acme", "prod").unwrap(),
            keys: &keys,
            export: &export,
            checkpoints: &DirWitness::new(self.path("witness")).all().unwrap(),
            interval_secs: 1,
            now: Some(gov_run::now()),
            gate_quiet_secs: None,
            clock_skew_secs: 60,
            retention_secs: None,
        })
    }
    /// Audit until every one of `receipts` is verified and witnessed, or give up.
    async fn audited(&self, receipts: u64) -> Report {
        for _ in 0..40 {
            let report = self.audit();
            let witnessed = report.gates.iter().all(|g| g.unwitnessed == 0);
            if report.ok() && report.receipts_verified == receipts && witnessed {
                return report;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let report = self.audit();
        panic!(
            "{} receipts verified, findings {:?}",
            report.receipts_verified, report.findings
        );
    }
}

async fn call(gate: &str, n: u64, key: &str) -> u16 {
    let body = json!({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": "jira_update_issue", "arguments": {"key": key, "summary": "Refund flow"}}});
    reqwest::Client::new()
        .post(format!("http://{gate}/mcp"))
        .header("authorization", SECRET)
        .header("x-gov-agent", "triage-bot")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// A file's text, and with it the text of everything in it that is base64: a receipt's body
/// is carried encoded, so a search of the raw file alone would miss what is inside it.
fn laid_bare(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    fn strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
            Value::Object(map) => map.values().for_each(|v| strings(v, out)),
            _ => {}
        }
    }
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        let mut found = Vec::new();
        strings(&value, &mut found);
        for s in found {
            if let Ok(decoded) = URL_SAFE_NO_PAD.decode(&s) {
                if std::str::from_utf8(&decoded).is_ok() {
                    text.push('\n');
                    text.push_str(&laid_bare(&decoded));
                }
            }
        }
    }
    text
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gate_and_a_ledger_run_as_programs_leave_a_record_an_auditor_can_check() {
    let Some(db) = Database::new().await else {
        return;
    };
    let site = Site {
        dir: tempfile::tempdir().unwrap(),
        db,
        upstream: serve(Router::new().route("/mcp", post(fake_jira))).await,
    };

    // Keys, made on the command line. The private key goes to a file only its owner can
    // read; what is printed is the public key and nothing else.
    let (ok, ledger_public, _) = run(site
        .ledger()
        .args(["keygen", "--kid", "ledger-k1", "--out"])
        .arg(site.path("ledger.key")));
    assert!(ok);
    let (ok, gate_public, _) = run(program(GATE)
        .args(["keygen", "--kid", "gate-1-k1", "--out"])
        .arg(site.path("gate.key")));
    assert!(ok);
    assert_eq!(mode(&site.path("gate.key")), 0o600);
    let key_file: Value =
        serde_json::from_slice(&std::fs::read(site.path("gate.key")).unwrap()).unwrap();
    assert!(!gate_public.contains(key_file["seed"].as_str().unwrap()));
    let ledger_public: PublicKey = serde_json::from_str(&ledger_public).unwrap();
    serde_json::from_str::<PublicKey>(&gate_public).unwrap();
    // A key is never written over another.
    let (ok, _, err) = run(program(GATE)
        .args(["keygen", "--kid", "gate-1-k2", "--out"])
        .arg(site.path("gate.key")));
    assert!(!ok && err.contains("must not exist"), "{err}");

    std::fs::write(site.path("gate.pub"), &gate_public).unwrap();
    std::fs::write(
        site.path("ledger-keys.json"),
        serde_json::to_vec(&[ledger_public]).unwrap(),
    )
    .unwrap();
    let map = ServerMap {
        server: id("jira-mcp"),
        system: id("jira"),
        fold_case: true,
        tools: vec![ToolMap {
            tool: id("jira_update_issue"),
            digest: tool_digest(&update_tool()).unwrap(),
            operation: id("issue.update"),
            kind: Kind::Write,
            reversal: Reversal::Restore,
            outward: false,
            resource: ResourceTemplate::new("jira/project/{key:before(-)}/issue/{key}").unwrap(),
        }],
    };
    std::fs::write(site.path("map.json"), serde_json::to_vec(&map).unwrap()).unwrap();

    // The gate is enrolled once; a second time is refused.
    let enrol = |site: &Site| {
        run(site
            .ledger()
            .args([
                "enrol", "--tenant", "acme", "--env", "prod", "--gate", "gate-1",
            ])
            .arg("--public-key")
            .arg(site.path("gate.pub")))
    };
    let (ok, out, err) = enrol(&site);
    assert!(ok && out.contains("enrolled key gate-1-k1"), "{out} {err}");
    assert!(!enrol(&site).0);

    let ledger = start({
        let mut c = site.ledger();
        c.args(["serve", "--tenant", "acme", "--env", "prod"])
            .args(["--listen", "127.0.0.1:0", "--checkpoint-interval", "1"])
            .arg("--key-file")
            .arg(site.path("ledger.key"))
            .arg("--witness")
            .arg(site.path("witness"));
        c
    });
    let ledger_url = format!("http://{}", ledger.addr);
    let mut gate = start(site.gate(&ledger_url));
    let operator = gate.said("operator on ");

    // An agent lists the tools and makes two calls. Nothing is blocked.
    let listing = reqwest::Client::new()
        .post(format!("http://{}/mcp", gate.addr))
        .header("authorization", SECRET)
        .body(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(listing.status().as_u16(), 200);
    assert_eq!(call(&gate.addr, 2, "PAY-12").await, 200);
    assert_eq!(call(&gate.addr, 3, "PAY-13").await, 200);

    // Within a few seconds the receipts are at the Ledger, under a witnessed checkpoint, and
    // an auditor's check of the files passes.
    let report = site.audited(4).await;
    assert_eq!(report.gates.len(), 1);

    // The gate says what mode it is in and what it recorded.
    // On the operator's address only: an agent that could read the counters would learn
    // when its calls go unrecorded (review finding GR-83).
    let agents_view = reqwest::get(format!("http://{}/healthz", gate.addr))
        .await
        .unwrap();
    assert_eq!(agents_view.status().as_u16(), 404);
    let health: Value = reqwest::get(format!("http://{operator}/healthz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (
            &health["mode"],
            &health["enforced"],
            &health["recorded"],
            &health["not_recorded"]
        ),
        (&json!("observe"), &json!(false), &json!(2), &json!(0))
    );

    // The gate is stopped and started again on the same spool: its chain goes on from where
    // it was, and the record is still whole.
    drop(gate);
    let gate = start(site.gate(&ledger_url));
    // A tool not listed since the restart is recorded as unknown, and still recorded.
    assert_eq!(call(&gate.addr, 4, "PAY-14").await, 200);
    site.audited(6).await;

    // Nothing the agent sent is in any file either program wrote: not its credential, not an
    // argument's value, not the server's answer.
    let mut files = vec![site.path("export.json")];
    for dir in ["spool", "witness"] {
        for entry in std::fs::read_dir(site.path(dir)).unwrap() {
            files.push(entry.unwrap().path());
        }
    }
    let mut written = String::new();
    for file in files.iter().filter(|f| f.is_file()) {
        // A file may be renamed away between the listing and the read; the export is not.
        if let Ok(bytes) = std::fs::read(file) {
            written.push_str(&laid_bare(&bytes));
        }
    }
    // The search reaches inside the receipts: what a receipt does hold is found.
    assert!(written.contains("triage-bot") && written.contains("issue.update"));
    for secret in ["agent-jira-token", "Refund flow", "customer 4411"] {
        assert!(
            !written.contains(secret),
            "{secret} is in a file the programs wrote"
        );
    }

    drop(gate);
    drop(ledger);
    site.db.drop_schema().await;
}

/// What each program refuses to start with.
#[test]
fn a_program_does_not_start_in_a_way_that_would_expose_a_key_or_a_credential() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name);
    let (ok, _, _) = run(program(GATE)
        .args(["keygen", "--kid", "gate-1-k1", "--out"])
        .arg(path("gate.key")));
    assert!(ok);
    let seed = serde_json::from_slice::<Value>(&std::fs::read(path("gate.key")).unwrap()).unwrap()
        ["seed"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::write(path("map.json"), "{}").unwrap();
    std::fs::write(path("ledger-keys.json"), "[]").unwrap();
    let gate = |listen: &str, upstream: &str, ledger: &str| {
        let mut c = program(GATE);
        c.args([
            "serve", "--tenant", "acme", "--env", "prod", "--gate", "gate-1",
        ])
        .args([
            "--listen",
            listen,
            "--upstream",
            upstream,
            "--ledger",
            ledger,
        ])
        .arg("--key-file")
        .arg(path("gate.key"))
        .arg("--spool")
        .arg(path("spool"))
        .arg("--map")
        .arg(path("map.json"))
        .arg("--ledger-keys")
        .arg(path("ledger-keys.json"));
        run(&mut c)
    };
    let local = "http://127.0.0.1:1/mcp";

    // The agent's credential passes through the gate: by default it does not listen where
    // another machine could reach it over plain HTTP, nor send plain HTTP to another machine.
    for (listen, upstream, ledger, says) in [
        ("0.0.0.0:0", local, local, "reachable from other machines"),
        (
            "127.0.0.1:0",
            "http://jira.example/mcp",
            local,
            "the upstream server is plain HTTP",
        ),
        (
            "127.0.0.1:0",
            local,
            "http://ledger.example",
            "the Ledger is plain HTTP",
        ),
        (
            "127.0.0.1:0",
            "ftp://127.0.0.1/mcp",
            local,
            "neither http nor https",
        ),
        // The name is whatever the resolver says it is; only a literal address is local.
        (
            "127.0.0.1:0",
            "http://localhost:1/mcp",
            local,
            "the upstream server is plain HTTP",
        ),
        (
            "127.0.0.1:0",
            "https://user:pw@jira.example/mcp",
            local,
            "holds no secret",
        ),
    ] {
        let (ok, _, err) = gate(listen, upstream, ledger);
        assert!(!ok && err.contains(says), "{listen} {upstream}: {err}");
        // A refusal never quotes a password from an address.
        assert!(!err.contains("pw@"), "{err}");
    }

    // Keys in files are for fake data only: without the operator's word that the data is
    // fake, no key is made and none is read.
    for class in [None, Some("real"), Some("")] {
        let mut make = Command::new(GATE);
        make.env_remove("GOV_DATA_CLASS")
            .args(["keygen", "--kid", "gate-1-k9", "--out"])
            .arg(path("other.key"));
        let mut read = Command::new(LEDGER);
        read.env_remove("GOV_DATA_CLASS")
            .args(["serve", "--tenant", "acme", "--env", "prod", "--key-file"])
            .arg(path("gate.key"))
            .arg("--witness")
            .arg(path("witness"));
        for command in [&mut make, &mut read] {
            if let Some(class) = class {
                command.env("GOV_DATA_CLASS", class);
            }
            let (ok, _, err) = run(command);
            assert!(!ok && err.contains("fake data only"), "{class:?}: {err}");
        }
        assert!(!path("other.key").exists());
    }

    // A key file that another account can read is not used, and nothing in it is printed.
    std::fs::set_permissions(path("gate.key"), std::fs::Permissions::from_mode(0o640)).unwrap();
    let (ok, out, err) = gate("127.0.0.1:0", local, local);
    assert!(!ok && err.contains("chmod 600"), "{err}");
    assert!(!out.contains(&seed) && !err.contains(&seed));
    // Nor is a link to a key file.
    std::fs::set_permissions(path("gate.key"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(path("gate.key"), path("link.key")).unwrap();
    let (ok, _, err) = run(program(LEDGER)
        .env(
            "GOV_LEDGER_DATABASE_URL",
            "postgresql://nobody:hunter2@127.0.0.1:1/none",
        )
        .args(["serve", "--tenant", "acme", "--env", "prod", "--key-file"])
        .arg(path("link.key"))
        .arg("--witness")
        .arg(path("witness")));
    assert!(!ok && err.contains("not a plain file"), "{err}");
    // A database that cannot be reached is said so without its address or password.
    let (ok, _, err) = run(program(LEDGER)
        .env(
            "GOV_LEDGER_DATABASE_URL",
            "postgresql://nobody:hunter2@127.0.0.1:1/none",
        )
        .args(["serve", "--tenant", "acme", "--env", "prod", "--key-file"])
        .arg(path("gate.key"))
        .arg("--witness")
        .arg(path("witness")));
    assert!(!ok && err.contains("cannot be reached"), "{err}");
    assert!(!err.contains("hunter2"), "{err}");
    // The Ledger's listener is held to the same rule about other machines.
    let (ok, _, err) = run(program(LEDGER)
        .args([
            "serve",
            "--tenant",
            "acme",
            "--env",
            "prod",
            "--listen",
            "0.0.0.0:0",
        ])
        .arg("--key-file")
        .arg(path("gate.key"))
        .arg("--witness")
        .arg(path("witness")));
    assert!(
        !ok && err.contains("reachable from other machines"),
        "{err}"
    );
}
