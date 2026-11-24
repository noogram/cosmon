// SPDX-License-Identifier: AGPL-3.0-only

//! The legacy runtime lands a completed molecule through the automatic entry
//! of `cs done`, never through the explicit one (ADR-192 I9; Refs #237).
//!
//! Only `cs done --if-completed` honours a `no-auto-harvest` reservation, and
//! `cs done <id>` is the gesture that lands a reserved molecule. A stand-in
//! `cs` behaves that way: given the explicit form it lands the molecule, given
//! the automatic form it prints `reserved` and lands nothing. The executor's
//! `on_complete` is observed through what the stand-in landed.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use cosmon_core::id::MoleculeId;
use cosmon_runtime::{Executor, SubprocessExecutor};

/// A stand-in `cs`: `done <id>` (explicit) writes `landed`, `done <id>
/// --if-completed` (automatic) leaves it absent, as the real entry does for a
/// reserved molecule.
fn reservation_honouring_cs(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("fake-cs.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             [ \"$1\" = done ] || exit 2\n\
             case \"$*\" in\n\
               *--if-completed*) echo reserved ;;\n\
               *) touch '{}/landed' ;;\n\
             esac\n\
             exit 0\n",
            dir.display()
        ),
    )
    .expect("write fake cs");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[test]
fn on_complete_does_not_land_a_reserved_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let exec = SubprocessExecutor::new(tmp.path())
        .quiet(true)
        .with_cs_program(reservation_honouring_cs(tmp.path()));

    exec.on_complete(&MoleculeId::new("task-20261124-aaaa").unwrap())
        .expect("the automatic entry answering `reserved` is not a failure");

    assert!(
        !tmp.path().join("landed").exists(),
        "on_complete used the explicit `cs done <id>`, which lands a tagged molecule"
    );
}
