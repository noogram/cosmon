// SPDX-License-Identifier: AGPL-3.0-only

//! The in-process loop budgets are configurable through `.cosmon/config.toml`
//! and resolve exact-model override > adapter field > harness default.

use cosmon_core::config::{LoopBudgetError, ProjectConfig};

fn adapter(toml_str: &str) -> cosmon_core::config::AdapterEntry {
    let cfg: ProjectConfig = toml::from_str(toml_str).expect("config parses");
    cfg.adapters.expect("adapters table").entries["openai"].clone()
}

/// A sparse row keeps every harness default.
#[test]
fn absent_budgets_resolve_to_defaults() {
    let a = adapter("[adapters.openai]\n");
    let r = a.loop_budget("gpt-x").unwrap();
    assert_eq!(r, Default::default());
    let b =
        cosmon_agent_harness::LoopBudget::new(r.max_turns, r.max_tool_calls, r.max_input_tokens)
            .unwrap();
    assert_eq!(b, cosmon_agent_harness::LoopBudget::DEFAULT);
}

/// The 30-turn budget the external team hit is raised from config.
#[test]
fn adapter_level_turn_budget_is_applied() {
    let a = adapter(
        "[adapters.openai]\nmax_turns = 80\nmax_input_tokens = 120000\nmax_tokens = 4096\n",
    );
    let r = a.loop_budget("gpt-x").unwrap();
    assert_eq!(r.max_turns, Some(80));
    assert_eq!(r.max_input_tokens, Some(120_000));
    assert_eq!(r.max_output_tokens, Some(4096));
}

/// An exact-model row wins per field; unset fields fall through; another
/// model id is untouched (no prefix matching).
#[test]
fn exact_model_override_precedence() {
    let a = adapter(
        "[adapters.openai]\nmax_turns = 80\nmax_tool_calls = 100\n\
         [adapters.openai.models.\"big\"]\nmax_turns = 200\n",
    );
    let big = a.loop_budget("big").unwrap();
    assert_eq!(big.max_turns, Some(200));
    assert_eq!(big.max_tool_calls, Some(100));
    assert_eq!(a.loop_budget("big-2").unwrap().max_turns, Some(80));
}

/// Zero and overflowing budgets are refused, not silently clamped.
#[test]
fn invalid_budgets_are_refused() {
    let zero = adapter("[adapters.openai]\nmax_turns = 0\n");
    assert_eq!(
        zero.loop_budget("m"),
        Err(LoopBudgetError::Zero { field: "max_turns" })
    );
    let zero_model = adapter("[adapters.openai.models.\"m\"]\nmax_input_tokens = 0\n");
    assert_eq!(
        zero_model.loop_budget("m"),
        Err(LoopBudgetError::Zero {
            field: "max_input_tokens"
        })
    );
    let overflow = adapter("[adapters.openai]\nmax_input_tokens = 4294967295\nmax_tokens = 2\n");
    assert!(matches!(
        overflow.loop_budget("m"),
        Err(LoopBudgetError::Overflow { .. })
    ));
}
