// SPDX-License-Identifier: AGPL-3.0-only

//! Shared helpers for the `cosmon-runtime` integration tests.
//!
//! Lives under `tests/common/` (a *subdirectory*, not a top-level
//! `tests/common.rs`) so Cargo treats it as a module to `mod`-include from
//! each test binary, **not** as its own test binary. Each integration test
//! that needs it declares `mod common;`.
//!
//! # Why this exists — the pyenv-shim subprocess tax
//!
//! The resident-runtime tests drive [`cosmon_runtime::RuntimeLoop`] against a
//! tiny Python "cs" stub that speaks the `ensemble` / `observe` / `tackle` /
//! `done` protocol. The loop shells out **once per verb per molecule** — a
//! three-molecule drain spawns the stub ~15 times (one `ensemble` read per
//! tick, plus `observe`/`tackle`/`done` per molecule).
//!
//! On a developer machine where `python3` resolves to a **pyenv shim**
//! (`~/.pyenv/shims/python3`), every spawn pays a 2–5 s version-resolution
//! tax inside `pyenv exec` *before* the interpreter even starts. Fifteen of
//! those push a sub-second drain past the 60 s `max_runtime` safety net — the
//! test does not deadlock, it is throttled by subprocess startup latency and
//! crawls to the deadline. The same shebang appears in
//! `resident_drain_dag.rs`, `resident_config_drift_halt.rs`, and
//! `sigint_race_suppresses_spurious_error.rs`, so the fix is factored here.
//!
//! Resolving the shim to its underlying interpreter **once**, at setup,
//! restores ~0.07 s/spawn and keeps the drain well under a second.

use std::fmt::Debug;
use std::path::Path;
use std::process::Command;

/// The message a missing interpreter fails with — see [`resolve_python3`].
const NO_PYTHON3: &str = "\
these resident-runtime tests drive the loop against a `python3` stub `cs`, and \
no usable python3 was found on this machine.\n\
Install one (Alpine/musl: `apk add python3`; Debian: `apt-get install -y \
python3`), or point $COSMON_TEST_PYTHON at an interpreter.\n\
Failing here rather than running is deliberate (issue #37): a stub whose \
interpreter cannot exec makes every `cs ensemble` spawn return ENOENT, the \
loop logs `ensemble-read-failed` on every tick, and the test spends 60 s \
waiting on a predicate that can never hold before reporting `Deadline` — a \
symptom that names neither the missing package nor the seam it broke.";

/// Resolve a fast, concrete `python3` interpreter, bypassing any pyenv shim.
///
/// Resolution order (first hit wins):
/// 1. `$COSMON_TEST_PYTHON` — explicit operator / CI override.
/// 2. `pyenv which python3` — unwraps a shim to the real binary it dispatches
///    to (the one-time `pyenv` spawn at setup is cheap; the per-loop spawns
///    then hit the resolved interpreter directly).
/// 3. Well-known absolute interpreters that no PATH shim can shadow.
/// 4. `/usr/bin/env python3` — last resort, the original shim-prone form, so
///    a machine without any of the above still runs (just slowly).
///
/// # Panics
///
/// When none of the four resolves to an interpreter that actually runs. That
/// is the whole point: see [`NO_PYTHON3`].
#[must_use]
pub fn resolve_python3() -> String {
    if let Ok(p) = std::env::var("COSMON_TEST_PYTHON") {
        if !p.is_empty() {
            return p;
        }
    }
    if let Ok(out) = Command::new("pyenv").args(["which", "python3"]).output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() && Path::new(&p).exists() {
                return p;
            }
        }
    }
    for cand in [
        "/usr/bin/python3",
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
    ] {
        if Path::new(cand).exists() {
            return cand.to_owned();
        }
    }
    assert!(
        interpreter_runs("/usr/bin/env", &["python3"]),
        "{NO_PYTHON3}"
    );
    "/usr/bin/env python3".to_owned()
}

/// Does `program` (with `leading` args) actually start a Python interpreter?
///
/// `Path::exists` is not enough for the `/usr/bin/env python3` form: `env`
/// always exists, and it is the *interpreter it looks up* that may not.
fn interpreter_runs(program: &str, leading: &[&str]) -> bool {
    let mut cmd = Command::new(program);
    cmd.args(leading).args(["-c", ""]);
    matches!(cmd.output(), Ok(out) if out.status.success())
}

/// Print the resident loop's decision trace when a run ended somewhere the
/// test did not expect.
///
/// Every one of these tests asserts a terminal [`ExitReason`]; when the
/// assertion fires, the reason alone (`Deadline`) names the *symptom* and
/// nothing else. The trace names the seam — it is one `decision_basis` per
/// tick, so a loop that never got off the ground says so on every line. That
/// is what turned issue #37 from "six tests die on musl" into "the stub could
/// not exec" in one run.
///
/// [`ExitReason`]: cosmon_runtime::ExitReason
pub fn dump_trace(trace_path: &Path, summary: &dyn Debug) {
    let trace = std::fs::read_to_string(trace_path).unwrap_or_default();
    eprintln!("=== TRACE ===\n{trace}\n=== END TRACE ===\nsummary: {summary:?}");
}

/// Rewrite a stub source's leading `#!/usr/bin/env python3` shebang to point
/// directly at the interpreter from [`resolve_python3`], dodging the pyenv
/// shim tax. A stub that does not start with that exact shebang is returned
/// verbatim (defensive — the caller's other `.replace(...)` substitutions
/// still apply).
#[must_use]
pub fn with_fast_python_shebang(stub_src: &str) -> String {
    let interp = resolve_python3();
    match stub_src.strip_prefix("#!/usr/bin/env python3\n") {
        Some(body) => format!("#!{interp}\n{body}"),
        None => stub_src.to_owned(),
    }
}
