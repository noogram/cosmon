// SPDX-License-Identifier: AGPL-3.0-only

//! Tiny helper binary used by `tests/trunk_lock_concurrent.rs` to acquire the
//! trunk lock from a *separate process* and hold it for a caller-supplied
//! duration. Models the real cross-process race the molecule is designed to
//! eliminate (two concurrent `cs done` against the shared cosmon main checkout).
//!
//! Args (positional):
//!   1. `state_dir` path
//!   2. `hold_ms` (`u64`)  — how long to keep the lock after acquisition
//!   3. `cmd_hint` string written into the holder file
//!
//! Stdout protocol (line-oriented so the parent can drive it deterministically):
//!   - `ACQUIRED <pid>\n` immediately after the lock is obtained
//!   - `RELEASED\n` after the hold duration elapses and the guard drops
//!
//! Lives under `examples/` so cargo builds it as a separate executable that
//! the integration test resolves via the `CARGO_BIN_EXE_*` env var family —
//! no `[[bin]]` entry needed and no impact on the main crate's API surface.

use std::path::PathBuf;
use std::time::Duration;

use cosmon_filestore::FileStore;

fn main() {
    use std::io::Write;

    let mut args = std::env::args().skip(1);
    let state_dir: PathBuf = args.next().expect("state_dir arg required").into();
    let hold_ms: u64 = args
        .next()
        .expect("hold_ms arg required")
        .parse()
        .expect("hold_ms must be u64");
    let cmd_hint = args
        .next()
        .unwrap_or_else(|| "trunk_lock_holder".to_owned());

    let store = FileStore::new(&state_dir);
    let guard = store
        .acquire_trunk_lock(&cmd_hint)
        .expect("acquire_trunk_lock failed");
    println!("ACQUIRED {}", std::process::id());
    std::io::stdout().flush().ok();

    std::thread::sleep(Duration::from_millis(hold_ms));
    drop(guard);
    println!("RELEASED");
}
