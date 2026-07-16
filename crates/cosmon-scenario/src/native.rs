// SPDX-License-Identifier: AGPL-3.0-only

//! Test-only native builtins for the scenario harness.
//!
//! Registered under the `cosmon::test::*` namespace to avoid overlap with
//! the production `cosmon::smoke::*` natives. These functions never touch
//! the filesystem or spawn processes — they only manipulate the in-memory
//! [`crate::Trace`].

use std::collections::HashMap;

use crate::ScenarioError;

/// Execution context for a native invocation.
pub struct NativeCtx {
    pub molecule: String,
    pub step_index: usize,
}

/// Outcome signalled by a native.
#[derive(Debug, Clone)]
pub enum Outcome {
    Ok,
    Fail(String),
    Record { tag: String, value: String },
}

pub type NativeFn = fn(&NativeCtx) -> Outcome;

/// Registry mapping `cosmon::test::*` keys to Rust functions.
pub struct NativeRegistry {
    fns: HashMap<&'static str, NativeFn>,
}

impl NativeRegistry {
    #[must_use]
    pub fn with_test_builtins() -> Self {
        let mut fns: HashMap<&'static str, NativeFn> = HashMap::new();
        fns.insert("cosmon::test::noop", noop);
        fns.insert("cosmon::test::fail", fail);
        fns.insert("cosmon::test::record", record);
        Self { fns }
    }

    pub fn call(&self, key: &str, ctx: &NativeCtx) -> Result<Outcome, ScenarioError> {
        let f = self
            .fns
            .get(key)
            .ok_or_else(|| ScenarioError::UnknownNative(key.into()))?;
        Ok(f(ctx))
    }
}

fn noop(_ctx: &NativeCtx) -> Outcome {
    Outcome::Ok
}

fn fail(ctx: &NativeCtx) -> Outcome {
    Outcome::Fail(format!(
        "native fail at {}/{}",
        ctx.molecule, ctx.step_index
    ))
}

fn record(ctx: &NativeCtx) -> Outcome {
    Outcome::Record {
        tag: ctx.molecule.clone(),
        value: format!("step:{}", ctx.step_index),
    }
}
