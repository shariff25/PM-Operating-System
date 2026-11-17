//! `pmos bot add-personal`: the personal bot alone, no `team.yaml` (docs/user-database-spec.md
//! §3: users live in the user database; the bot stays in content).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_pmos");

fn run(root: &std::path::Path, handle: &str) -> std::process::Output {
    Command::new(BIN)
        .args(["bot", "add-personal", "--root"])
        .arg(root)
        .args(["--handle", handle])
        .output()
        .expect("run pmos")
}

#[test]
fn writes_the_personal_bot_and_never_team_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let out = run(root, "ana");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bot = std::fs::read_to_string(root.join("bots/pm-ana/BOT.md")).unwrap();
    assert!(
        bot.contains("name: pm-ana") && bot.contains("owner: \"ana\""),
        "{bot}"
    );
    assert!(!root.join("team.yaml").exists(), "no team.yaml is written");

    // Again: nothing changes, and it says so.
    std::fs::write(root.join("bots/pm-ana/BOT.md"), "edited\n").unwrap();
    let out = run(root, "ana");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("already there"));
    assert_eq!(
        std::fs::read_to_string(root.join("bots/pm-ana/BOT.md")).unwrap(),
        "edited\n"
    );

    // A handle that can't be a bot name: refused, nothing written.
    for bad in ["Ana", "bots.x", "a--b", "../x"] {
        let out = run(root, bad);
        assert!(!out.status.success(), "{bad}");
    }
    assert_eq!(std::fs::read_dir(root.join("bots")).unwrap().count(), 1);
}
