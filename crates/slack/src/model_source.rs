//! `pmos-slack` and the model source (slice H4, `docs/openshell-any-host-spec.md` §3.6,
//! "hosted-test, and fake-data mode"): `PMOS_MODEL_SOURCE` is required, and under
//! `hosted-test` (fake data only) `pmos-slack` refuses to start with Slack tokens set. A test
//! workspace can't be told apart from a real one, so in fake-data mode the Slack bot doesn't
//! run at all.

use pmos_contract::model_source::{fake_data_refusal, ModelSource};

/// The source, or why `pmos-slack` won't start with these settings.
pub fn check(get: impl Fn(&str) -> Option<String>) -> Result<ModelSource, String> {
    let source = ModelSource::from_lookup(&get)?;
    if source.fake_data_only() {
        let set: Vec<&str> = ["PMOS_SLACK_APP_TOKEN", "PMOS_SLACK_BOT_TOKEN"]
            .into_iter()
            .filter(|k| get(k).is_some_and(|v| !v.trim().is_empty()))
            .collect();
        if !set.is_empty() {
            return Err(fake_data_refusal(&format!(
                "{} must be blank: a test Slack workspace can't be told apart from a real one, \
                 so pmos-slack doesn't run in fake-data mode",
                set.join(" and ")
            )));
        }
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pairs: &[(&str, &str)]) -> Result<ModelSource, String> {
        check(|k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn slack_tokens_are_refused_in_fake_data_mode() {
        let tokens = [
            ("PMOS_SLACK_APP_TOKEN", "xapp-1-test"),
            ("PMOS_SLACK_BOT_TOKEN", "xoxb-test"),
        ];
        let mut local = tokens.to_vec();
        local.push(("PMOS_MODEL_SOURCE", "local"));
        assert_eq!(run(&local), Ok(ModelSource::Local));
        let mut fake = tokens.to_vec();
        fake.push(("PMOS_MODEL_SOURCE", "hosted-test"));
        let err = run(&fake).unwrap_err();
        assert!(
            err.starts_with(
                "PMOS_MODEL_SOURCE=hosted-test sends prompts to an endpoint nobody approved"
            ),
            "{err}"
        );
        assert!(
            err.contains("PMOS_SLACK_APP_TOKEN and PMOS_SLACK_BOT_TOKEN must be blank"),
            "{err}"
        );
        let err = run(&[
            ("PMOS_MODEL_SOURCE", "hosted-test"),
            ("PMOS_SLACK_BOT_TOKEN", "xoxb-test"),
        ])
        .unwrap_err();
        assert!(err.contains("PMOS_SLACK_BOT_TOKEN must be blank"), "{err}");
        assert_eq!(
            run(&[("PMOS_MODEL_SOURCE", "hosted-test")]),
            Ok(ModelSource::HostedTest)
        );
        assert!(run(&tokens)
            .unwrap_err()
            .contains("PMOS_MODEL_SOURCE is required"));
        assert!(run(&[("PMOS_MODEL_SOURCE", "hosted")])
            .unwrap_err()
            .contains("not built yet"));
    }
}
