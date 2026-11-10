// SPDX-License-Identifier: AGPL-3.0-only

//! Pure rendering of the task text carried by a molecule briefing.

use std::collections::HashMap;
use std::fmt::Write;
use std::hash::BuildHasher;

use crate::config::GatesConfig;

/// Render the project's gate commands for both durable briefs and worker prompts.
///
/// Commands come only from the galaxy's configuration. An undeclared toolchain
/// is left to the project's documentation rather than inferred from a formula.
#[must_use]
pub fn render_gates(gates: &GatesConfig) -> String {
    if gates.is_empty() {
        return "the project's own test, lint and build gates \
                (see its CLAUDE.md / CONTRIBUTING).\n"
            .to_owned();
    }

    let mut text =
        String::from("the project's verification gates (from .cosmon/config.toml `[gates]`):\n");
    for (label, command) in [
        ("setup", &gates.setup_command),
        ("build", &gates.build_command),
        ("typecheck", &gates.typecheck_command),
        ("test", &gates.test_command),
        ("lint", &gates.lint_command),
        ("format", &gates.format_command),
        ("doc", &gates.doc_command),
    ] {
        if let Some(command) = command {
            let _ = writeln!(text, "   - {label}: `{command}`");
        }
    }
    text
}

/// Render every bound formula variable in a stable order for `briefing.md`.
///
/// The topic leads because it is the task statement. Values are copied
/// verbatim so the durable brief can reconstruct the task without a pane.
#[must_use]
pub fn render_task<S: BuildHasher>(variables: &HashMap<String, String, S>) -> String {
    if variables.is_empty() {
        return String::new();
    }

    let mut text = String::from("## Task\n\n");
    if let Some(topic) = variables.get("topic") {
        let _ = write!(text, "### topic\n\n{topic}\n\n");
    }
    let mut keys: Vec<_> = variables
        .keys()
        .filter(|key| key.as_str() != "topic")
        .collect();
    keys.sort();
    for key in keys {
        let _ = write!(text, "### {key}\n\n{}\n\n", variables[key]);
    }
    text
}
