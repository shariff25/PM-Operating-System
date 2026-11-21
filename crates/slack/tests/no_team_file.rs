//! No decision path in the Slack service reads the team file (slice U4b,
//! `docs/user-database-spec.md` §12 "Proving no service decides from the file"): its parser,
//! its types and every lookup on them are gone from the service's code, and `team.yaml` is
//! named only in comments that say it isn't read. Who someone is comes from the user directory
//! (`src/people.rs`); the socket tests (`socket_onboarding.rs`) show a hostile or absent
//! `team.yaml` changes nothing.

use std::path::Path;

#[test]
fn the_slack_service_reads_no_team_file() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = 0;
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        files += 1;
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            for banned in [
                "TeamFile",
                "TeamMember",
                "load_team",
                "team_file",
                "team.yaml",
                ".team()",
                "by_handle(",
                "from_yaml",
                "PMOS_TEAM",
            ] {
                assert!(
                    !code.contains(banned),
                    "{}:{}: `{banned}` in the Slack service's code",
                    path.display(),
                    n + 1
                );
            }
        }
    }
    assert!(files > 20, "read the service's sources ({files} files)");
}
