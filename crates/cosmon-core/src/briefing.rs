// SPDX-License-Identifier: AGPL-3.0-only

//! Pure rendering of the task text carried by a molecule briefing.

use std::collections::HashMap;
use std::fmt::Write;
use std::hash::BuildHasher;

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
