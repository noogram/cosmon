<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Spawn matrix contract

`run_matrix.sh` observes session, live pane, branch, worktree and fleet entry
(`SPBWF`) after `tackle` returns, before scratch cleanup. A failed spawn must
leave `00000` and return nonzero. These are postconditions, not records of
which resources existed temporarily during dispatch.

The source contract is the symmetry comment at the `spawn_and_prompt` match
in `crates/cosmon-cli/src/cmd/tackle.rs`, its `rollback_dispatch` calls and
`cleanup_partial_tackle`. The dispatch boundary in
`docs/architectural-invariants.md` §8v M1 requires positive liveness before
briefing; a dead or unobservable pane cannot be admitted. The transport port
separates `is_alive` from `session_exists` and `terminate_session`: teardown
must reach retained dead panes without changing the liveness filter.

| Rows | Historical expectation | Contract enforced now |
|---|---|---|
| `tmux-new-fail`, `new-fail+auth` | `00110`, nonzero | `00000`, nonzero: undo the branch and worktree after failed creation |
| `tmux-pane-dead`, `tmux-session-exited`, `pane-dead+hang`, `session-exited+partial`, `pane-dead+exit-delayed`, `pane-dead+auth` | `10111`, zero | `00000`, nonzero: refuse the dead pane, roll back bookkeeping, reclaim its session |
| `tmux-list-empty`, `list-empty+segfault` | `11110`, nonzero | `00000`, nonzero: lack of positive liveness refuses dispatch and rolls back all resources |
| `tmux-no-server`, `no-server+exit-42` | `?????`, arbitrary | Unscored observations, reported separately from passes and failures |

The historical expectations predate transactional failed-spawn cleanup and
the positive-liveness gate. Baseline measurements alone do not authorize an
oracle change: the named source contracts and the falsifiers below do.

`FAKE_TMUX_LIST_EMPTY` and `FAKE_TMUX_NO_SERVER` hide global listings while
retaining targeted session access and metadata. They test disagreement
between observations, not a real server disappearing. Fake tmux does not
fork a provider in this matrix; the Claude mode labels do not qualify real
provider exit codes, authentication or crashes. The nine `11111` success rows
qualify the fake transport surface only.

## Regressions and falsifiers

Run commands through `scripts/no-pilot-env.sh`. Build an explicit `CS_BIN`
from the source being tested. Use a fresh absolute `MATRIX_OUTDIR` for every
run; a nonempty directory is refused. `MATRIX_ROW` selects one exact row, and
an unknown selection is a harness error. Setup/cleanup errors are distinct
from scored failures. The harness stops only observers whose binary,
molecule and scratch paths agree before removing its fake sessions and
scratch repo. `observed-state` and `observed-tmux-state` retain the pre-cleanup
state. Cleanup calls are marked in the complete tmux trace.

- `cargo test -p cosmon-cli --bin cs real_dead_pane -- --test-threads=1 --nocapture`
  exercises both actual tackle cleanup functions against real tmux on private
  sockets. It checks dead-pane absence, a second cleanup with the target absent,
  and survival of a session whose name extends the target. Revert each cleanup
  call from `terminate_session(wid.as_str())` to `terminate(wid)`: its absence
  assertion must fail. The live-list filter stays unchanged.
- With those two calls reverted, run `MATRIX_ROW=tmux-pane-dead` and
  `MATRIX_ROW=tmux-list-empty`. The retained session must fail `00000`, even
  though the liveness gate still returns nonzero and rolls back B/W/F.
- In a disposable source copy, omit `cleanup_partial_worktree` from
  `cleanup_partial_tackle`, rebuild, then run `MATRIX_ROW=tmux-new-fail` (or
  `new-fail+auth`). Retained B/W must fail `00000`. Restore the call and verify
  that the row passes again. This pins the revised rollback expectation.
- Remove exact-name targeting from both presence and teardown in the existing
  tmux recovery port on a disposable copy. The absent-target/prefixed-neighbor
  regression must reject redirecting a teardown to that neighbor. Pinned
  backends must continue through `tmux_cmd` / `terminal::bound_command`; no
  name/PID fallback is authorized.
