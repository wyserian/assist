//! Runs every case in `config/cases.toml` against the example config and default phrases.

use std::collections::BTreeMap;

use assist_core::config::Config;
use assist_core::matcher::{Matcher, RuleMatcher};
use assist_core::phrases::PhraseSet;
use serde::Deserialize;

#[derive(Deserialize)]
struct Cases {
    case: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    text: String,
    command: Option<String>,
    #[serde(default)]
    slots: BTreeMap<String, String>,
}

#[test]
fn phrase_cases() {
    let config = Config::from_toml(include_str!("../../../config/config.example.toml")).unwrap();
    let phrases = PhraseSet::default_en().unwrap();
    config.validate_commands(&phrases).unwrap();
    let matcher = RuleMatcher::new(&phrases, &config).unwrap();
    let cases: Cases = toml::from_str(include_str!("../../../config/cases.toml")).unwrap();

    let failures: Vec<String> = cases
        .case
        .iter()
        .filter_map(|case| {
            let want = case.command.clone().map(|c| (c, case.slots.clone()));
            let got = matcher.best(&case.text).map(|i| (i.command, i.slots));
            (got != want).then(|| format!("{:?}\n    want {want:?}\n    got  {got:?}", case.text))
        })
        .collect();

    assert!(cases.case.len() >= 20, "expected at least 20 cases");
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        cases.case.len(),
        failures.join("\n")
    );
}
