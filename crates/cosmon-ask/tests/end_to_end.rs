// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end integration tests — exercise the pipeline against the
//! real `cosmon-registry::TomlGalaxyIndex`, not a mocked backend.
//!
//! The shell-out to `cs nucleate` / `cs tackle` is out of scope here;
//! that is the CLI handler's job. These tests confirm that a
//! hand-authored `galaxies.toml` + a free-text sentence flow cleanly
//! into an `AskState::Resolved` or `AskState::AskedClarification`.

use cosmon_ask::{AskPipeline, AskState, RuleParser};
use cosmon_registry::TomlGalaxyIndex;

#[test]
fn rule_path_resolves_against_toml_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("galaxies.toml");
    std::fs::write(
        &cfg,
        r#"
[[galaxy]]
name = "mailroom"
path = "/tmp/mailroom"
default_formulas = { issue = "task-work" }

[[galaxy]]
name = "cosmon"
path = "/tmp/cosmon"
"#,
    )
    .unwrap();

    let registry = TomlGalaxyIndex::load_from(&cfg).unwrap();
    let pipe = AskPipeline::new(RuleParser::with_defaults(), registry);

    match pipe.run("fix the bug in mailroom").unwrap() {
        AskState::Resolved {
            galaxy, formula, ..
        } => {
            assert_eq!(galaxy.name, "mailroom");
            assert_eq!(formula.as_str(), "task-work");
        }
        other => panic!("expected Resolved, got {other:?}"),
    }
}

#[test]
fn twenty_canonical_intents_all_match() {
    // Exercise the headline intents from the briefing so the table is
    // self-documenting — adding a new rule without a sample here is
    // the failure mode this test catches.
    let cases = [
        ("fix the mailroom bug", true),
        ("patch the crash we just hit", true),
        ("debug the failing ci", true),
        ("ship the current mailroom branch", true),
        ("deploy the new ask verb", true),
        ("release v0.3", true),
        ("triage my open bugs", true),
        ("review the pending delibs", true),
        ("audit the surfaces", true),
        ("plan the cosmon-node rollout", true),
        ("design the ask pipeline", true),
        ("architect the voice ingress", true),
        ("deliberate on cs ask with jobs", true),
        ("panel the urgency question", true),
        ("chronicle today's syzygie event", true),
        ("write the ADR for ask", true),
        ("draft the ADR for ask", true),
        ("refactor the event handlers", true),
        ("explore idempotency in reconcile", true),
        ("map the alert surfaces", true),
    ];
    let p = RuleParser::with_defaults();
    for (text, should_match) in cases {
        let (tokens, confidence) = cosmon_ask::Parser::parse(&p, text).unwrap();
        assert!(
            confidence > 0.0,
            "expected verb match for `{text}` but got confidence 0 (tokens={tokens:?})"
        );
        assert_eq!(should_match, confidence > 0.0, "case: {text}");
    }
}

#[test]
fn low_confidence_on_out_of_vocabulary_input() {
    let registry = TomlGalaxyIndex::empty();
    let pipe = AskPipeline::new(RuleParser::with_defaults(), registry);
    let state = pipe.run("foobar the thingamajig").unwrap();
    matches!(state, AskState::AskedClarification { .. });
}

#[test]
fn empty_input_errors() {
    let registry = TomlGalaxyIndex::empty();
    let pipe = AskPipeline::new(RuleParser::with_defaults(), registry);
    assert!(pipe.run("").is_err());
    assert!(pipe.run("    ").is_err());
}
