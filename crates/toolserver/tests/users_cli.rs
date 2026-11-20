//! `pmos-toolserver users …`, the host commands, as the binary runs them: refused without
//! `PMOS_HOST_USER` (or with a value that isn't an OS user name) before anything else, and
//! the host user recorded in `role_changes` and the audit log. With `PMOS_TEST_DATABASE_URL`
//! for the cases that write.

mod common;

use std::process::{Command, Output};

use common::TestPg;

const BIN: &str = env!("CARGO_BIN_EXE_pmos-toolserver");

fn users(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.arg("users").args(args).env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run pmos-toolserver")
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

const BOOTSTRAP: [&str; 10] = [
    "bootstrap-admin",
    "--handle",
    "raj",
    "--sign-in-name",
    "raj@contoso.com",
    "--atlassian-email",
    "raj@contoso.com",
    "--slack-id",
    "U0RAJ",
    "--no-entra",
];

#[test]
fn refused_without_a_usable_host_user_before_anything_else() {
    // No PMOS_HOST_USER: exit 2, and the database (unreachable here) is never tried.
    let unreachable = [("PMOS_DATABASE_URL", "postgresql://nobody@127.0.0.1:1/none")];
    let o = users(&BOOTSTRAP, &unreachable);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(2), "{err}");
    assert!(err.contains("PMOS_HOST_USER is required"), "{err}");
    for bad in ["Root User", "root;rm", "", "ROOT", "a".repeat(40).as_str()] {
        let o = users(&["list"], &[unreachable[0], ("PMOS_HOST_USER", bad)]);
        assert_eq!(o.status.code(), Some(2), "{bad:?}");
    }
    // No database: exit 2 with what to do.
    let o = users(&["list"], &[("PMOS_HOST_USER", "opsuser")]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(2));
    assert!(err.contains("PMOS_DATABASE_URL"), "{err}");
    // Argument errors: exit 2.
    for args in [
        &["grant-admin", "raj"][..],
        &["grant-admin", "raj", "--oid", "x", "--keep-entra"],
        &["bootstrap-admin", "--handle", "raj"],
        &[
            "import",
            "--from",
            "team.yaml",
            "--bootstrap",
            "--allow-admins",
        ],
        &["pilot-exception", "admin-and-auditor", "on"],
        &["export"],
        &["nonsense"],
    ] {
        let o = users(args, &[("PMOS_HOST_USER", "opsuser"), unreachable[0]]);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o).1);
    }
    // --oid without a tenant: exit 2, before the database.
    let o = users(
        &[
            "set-entra",
            "raj",
            "--oid",
            "aaaaaaaa-0000-0000-0000-000000000001",
        ],
        &[("PMOS_HOST_USER", "opsuser"), unreachable[0]],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).1.contains("PMOS_ENTRA_TENANT_ID"));
    let o = users(&["--help"], &[]);
    assert_eq!(o.status.code(), Some(0));
    assert!(text(&o).0.contains("bootstrap-admin"));
}

#[tokio::test]
async fn host_commands_record_the_host_user() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let url = pg.schema_url();
    let env = [
        ("PMOS_HOST_USER", "opsuser"),
        ("PMOS_DATABASE_URL", url.as_str()),
        (
            "PMOS_ENTRA_TENANT_ID",
            "11111111-2222-3333-4444-555555555555",
        ),
    ];
    let o = users(&BOOTSTRAP, &env);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}{err}");
    assert!(out.contains("recorded as host user opsuser"), "{out}");
    // Once is all: exit 3.
    let o = users(&BOOTSTRAP, &env);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(3), "{err}");
    assert!(err.contains("refused (admin_exists)"), "{err}");
    // A refusal by the rules is 3 too: the last admin.
    let o = users(&["revoke-admin", "raj"], &env);
    assert_eq!(o.status.code(), Some(3));
    assert!(text(&o).1.contains("last_admin"));
    let o = users(
        &[
            "set-entra",
            "raj",
            "--oid",
            "AAAAAAAA-0000-0000-0000-000000000001",
        ],
        &env,
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let o = users(&["list"], &env);
    let (out, _) = text(&o);
    assert!(
        out.contains("raj") && out.contains("admin, pm") && out.contains("entra:linked"),
        "{out}"
    );
    let o = users(&["pilot-exception", "admin-and-auditor", "status"], &env);
    assert!(text(&o).0.contains("off"));

    let host_users: Vec<Option<String>> = sqlx::query_scalar("SELECT host_user FROM role_changes")
        .fetch_all(pg.store.pool())
        .await
        .unwrap();
    assert!(!host_users.is_empty());
    assert!(host_users.iter().all(|h| h.as_deref() == Some("opsuser")));
    let details: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT tool, client, detail FROM audit_events ORDER BY id")
            .fetch_all(pg.store.pool())
            .await
            .unwrap();
    assert_eq!(details.len(), 4, "{details:?}");
    for (tool, client, detail) in &details {
        assert!(tool.starts_with("users."), "{tool}");
        assert_eq!(client, "host");
        assert!(
            detail.as_deref().unwrap().starts_with("host_user opsuser"),
            "{detail:?}"
        );
        assert!(
            !detail.as_deref().unwrap().contains("aaaaaaaa"),
            "no oid in the log"
        );
    }

    // The import: a dry run by default; --bootstrap on a non-empty table exits 3.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("team.yaml");
    std::fs::write(
        &file,
        "members:\n  - handle: raj\n    slack-id: U0RAJ\n    email: raj@contoso.com\n    roles: [pm, admin]\n  - handle: ana\n    slack-id: U0ANA\n    email: ana@contoso.com\n    roles: [pm]\n",
    )
    .unwrap();
    let from = file.to_str().unwrap();
    let o = users(&["import", "--from", from, "--commit", "abc1234"], &env);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(
        out.contains("create     `ana`") && out.contains("dry run"),
        "{out}"
    );
    let o = users(&["import", "--from", from, "--bootstrap", "--apply"], &env);
    assert_eq!(o.status.code(), Some(3));
    assert!(text(&o).1.contains("not_empty"));
    // --allow-admins with admins to create needs a terminal: refused here (no terminal),
    // nothing written. (raj exists, so the file's admin is ana's only to create.)
    std::fs::write(
        &file,
        "members:\n  - handle: ana\n    slack-id: U0ANA\n    email: ana@contoso.com\n    roles: [pm, admin]\n",
    )
    .unwrap();
    let o = users(
        &["import", "--from", from, "--allow-admins", "--apply"],
        &env,
    );
    assert_eq!(o.status.code(), Some(2), "{}", text(&o).1);
    assert!(text(&o).1.contains("terminal"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pg.store.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
    // Without --allow-admins it applies, and ana comes in without admin.
    let o = users(&["import", "--from", from, "--apply"], &env);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let o = users(&["export", "--format", "team-yaml"], &env);
    let (out, _) = text(&o);
    assert!(
        out.contains("handle: \"ana\"") && out.contains("roles: [pm]"),
        "{out}"
    );
    pg.drop_schema().await;
}

/// `password-setup` (docs/password-sign-in.md): the host issues a setup code, for an Admin too,
/// shows it once, and records the host user; the code itself is in no log or audit event.
#[tokio::test]
async fn the_host_issues_a_password_setup_code() {
    let Some(pg) = TestPg::new().await else {
        return;
    };
    let url = pg.schema_url();
    let env = [
        ("PMOS_HOST_USER", "opsuser"),
        ("PMOS_DATABASE_URL", url.as_str()),
    ];
    let o = users(&BOOTSTRAP, &env);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    // raj is an Admin: only the host may issue his code.
    let o = users(&["password-setup", "raj"], &env);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}{err}");
    assert!(out.contains("setup code for `raj`"), "{out}");
    assert!(out.contains("recorded as host user opsuser"), "{out}");
    let code = out
        .lines()
        .find(|l| l.len() == 23 && l.matches('-').count() == 3)
        .expect("the code, on a line of its own")
        .to_string();
    // A refusal by the rules: no such user, exit 3.
    let o = users(&["password-setup", "nobody"], &env);
    assert_eq!(o.status.code(), Some(3));
    assert!(text(&o).1.contains("refused (not_found)"));
    // Without a host user, nothing runs.
    let o = users(&["password-setup", "raj"], &[env[1]]);
    assert_eq!(o.status.code(), Some(2));
    let (host_user, change, detail): (Option<String>, String, Option<String>) = sqlx::query_as(
        "SELECT host_user, change, detail FROM role_changes WHERE change = 'issue_password_code'",
    )
    .fetch_one(pg.store.pool())
    .await
    .unwrap();
    assert_eq!(
        (host_user.as_deref(), change.as_str(), detail.as_deref()),
        (Some("opsuser"), "issue_password_code", Some("setup"))
    );
    let details: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT detail FROM audit_events WHERE tool = 'password.setup_code' ORDER BY id",
    )
    .fetch_all(pg.store.pool())
    .await
    .unwrap();
    assert_eq!(
        details,
        vec![
            Some("host_user opsuser; setup".to_string()),
            Some("host_user opsuser; not_found".to_string())
        ]
    );
    let stored: Vec<String> = sqlx::query_scalar("SELECT code_hash FROM password_setup_codes")
        .fetch_all(pg.store.pool())
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].contains(&code) && stored[0].len() == 64);
    pg.drop_schema().await;
}
