<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Setting up a galaxy for pilot transfer, and moving the controls between providers

Governed by [ADR-168](../adr/168-a-co-pilot-inherits-the-session-substrate-not-its-delivery-contract.md)
and [ADR-171](../adr/171-the-operator-gesture-is-a-signature-not-a-string.md).
This page is the operator's sequence. The signature itself, and what to do when
it is refused, are in [takeover-operator-gesture.md](takeover-operator-gesture.md);
how to undo a transfer is in [takeover-rollback.md](takeover-rollback.md).

The case covered here: a Claude or Codex session holds the PRIMARY seat on a
mission, and you want the other provider to hold it, with one signature from
you and no command typed by an agent on your behalf.

## One-time setup per galaxy

Do these four things once. None of them is repeated per transfer.

**1. Commit the operator public key as `.cosmon/takeover.pub`.** This is the
trust root: a grant is honoured only if this key signed it. Generation, the
passphrase advice and the reason it must be tracked are in
[takeover-operator-gesture.md](takeover-operator-gesture.md#one-time-setup-pin-your-key).
Check what is in force with `cs sessions takeover trust`. With no pinned key,
no transfer is possible.

**2. Create one ordinary task molecule tagged `pilot-mission`.**

```sh
cs nucleate task-work --var topic="pilot mission" --tag pilot-mission
```

Its id is the mission id used in every `--mission` below. `cs sessions seats`
treats every molecule carrying this tag, and every mission that already has a
lease ledger, as a mission of the galaxy. Two such missions give the galaxy the
state `ambiguous mission`, so keep one. The molecule is an ordinary task; the
tag is the only thing that marks it (see the drain behaviour below).

**3. Install the provider hooks once, at user level.**

```sh
cs sessions hook install --provider claude --settings ~/.claude/settings.json
cs sessions hook install --provider codex
```

Codex needs no `--settings`: the default is its user configuration,
`$CODEX_HOME/config.toml` or `~/.codex/config.toml`. Claude's default is the
project-local `.claude/settings.local.json`, so the explicit `--settings` above
is what makes the install user-level. Add `--dry-run` to print what would be
written. An existing Codex `notify` held by another program is chained behind,
not replaced.

The hooks are not part of `cs sessions launch`, which only mints the session
identity. They must already be installed when the new session starts. Outside a
cosmon galaxy a hook succeeds and writes nothing.

**4. Trust the galaxy in Codex, separately.** Codex keeps its own per-directory
trust decision, recorded in `~/.codex/config.toml` as:

```toml
[projects."/path/to/galaxy"]
trust_level = "trusted"
```

This is a different gesture from the takeover signature and neither implies the
other. Your signature moves the PRIMARY seat and says nothing about whether
Codex may work in the directory; Codex's trust says nothing about who holds the
seat. Cosmon neither performs nor checks the Codex gesture. Make it yourself,
once, when you first open the galaxy in Codex, and confirm it in that file.

## The transfer

Seven steps. Steps 1, 3 and 6 are done by sessions, steps 2, 4 and 7 by Pupitre, and
step 5 by you. Pupitre is the launcher that
opens provider surfaces; it is not part of this repository and everything below
is the contract it relies on.

1. **The outgoing primary publishes a checkpoint.** The pilot writes a
   `cosmon.checkpoint-draft/v1` document and publishes it:

   ```sh
   cs sessions checkpoint publish --mission <MISSION> --from-file draft.json
   ```

   Provenance (session and lease epoch) is stamped from the verified lease, not
   from the draft.

2. **Pupitre opens the target provider from a first-prompt file.** It starts
   `cs sessions launch <claude|codex> -- <ARGS>`, which mints a fresh anonymous
   `COSMON_SESSION_ID` shared by the hooks and by the tools the session runs.
   The first prompt comes from the file and tells the session which mission to
   join.

3. **The new session attaches as copilot, reads the checkpoint and asks.**

   ```sh
   cs sessions attach --role copilot --mission <MISSION> --follow <OUTGOING_SID>
   cs sessions checkpoint show --mission <MISSION> --of-holder --json
   cs sessions takeover request --mission <MISSION> --reason '<why>'
   ```

   `--of-holder` returns `{checkpoint, selected_by}` for the newest record
   published by the verified lease holder at its current epoch. A request
   confers nothing.

4. **Pupitre displays the exact grant command and the challenge preview.** The
   grant command is

   ```sh
   cs sessions takeover grant --mission <MISSION> --request <REQ> \
       --by <OPERATOR> --sign-with <KEY>
   ```

   and the preview is the six signed lines printed by

   ```sh
   cs sessions takeover challenge --mission <MISSION> --request <REQ> --by <OPERATOR>
   ```

   `<OPERATOR>` must be the same in both: `granted_by=` is inside the signed
   bytes, and `--by` defaults to `$USER` when omitted. Pupitre does not run
   either command and does not read the private key.

5. **You run that one command and answer the signer prompt once.** `cs` prints
   the six lines it is about to have signed, `minisign` asks for your
   passphrase, and the grant is written. Read the lines before typing; they are
   the whole transfer. Use the `--attestation` form from
   [takeover-operator-gesture.md](takeover-operator-gesture.md) when the key is
   on another machine.

6. **The new pilot seats itself on its next ordinary turn.** Nobody types it
   for the pilot. The pilot runs

   ```sh
   cs sessions attach --role primary --mission <MISSION> --epoch <EPOCH>
   ```

   with the epoch printed in the grant (the `epoch=` line you signed), and the
   ledger accepts the claim because the grant names this session at that epoch.
   The hook does not do this for it: a hook never claims a seat. The outgoing
   session needs to do nothing; its next ordinary heartbeat reads the ledger,
   finds a higher epoch and demotes itself to copilot
   ([takeover-rollback.md](takeover-rollback.md)).

7. **Pupitre closes the old surface only after `sessions seats --json` reports
   exactly one primary.**

   ```sh
   cs sessions seats --json
   ```

   The mission's `verdict` is one of:

   | verdict | meaning | close the old surface? |
   |---|---|---|
   | `one` | the verified holder is seated and no other session claims primary | yes |
   | `granted-unseated` | a grant exists, nobody holds the seat yet | no, wait for step 6 |
   | `contested` | the holder is seated and another session claims primary at the lease epoch or with a claim the lease cannot verify | no |
   | `none`, `epoch conflict` | no verified holder, or two holders at the top epoch | no |

   Authority comes from the lease ledger, never from heartbeat age: until the
   outgoing session's heartbeat demotes it, its primary claim at the older
   epoch is shown as `superseded` and the verdict is already `one`: the
   ledger backs only the incoming holder. A superseded claim from a session
   that never beats again stays visible but never contests. `contested` is
   kept for a second claim the lease cannot rule out: same epoch, a higher
   epoch, or no epoch. Pupitre therefore reads the verdict and does not close
   on a timer. A Codex session counts as
   seated only when its presence carries the native Codex thread id; without it
   the verdict stays `granted-unseated`.

## Limits in V1

- Authority is scoped to the `pilot-mission` molecule. A grant is a lease on
  that mission; it does not extend to other molecules or to the rest of the
  galaxy.
- Live Codex lifecycle through the app server (issue #193) is additive. This
  sequence does not depend on it and does not assume it.
- A missing checkpoint stays missing. `--of-holder` matches the exact holder
  and epoch; if no such record exists, `checkpoint` is `null` in the result and
  the command does not return an older record. The new session starts from the
  null, not from a previous holder's note.

## How a long-lived `pilot-mission` task appears to drain sweeps

Observed on an isolated fixture galaxy (a scratch git repository, a fresh
`cs init`, two `task-work` molecules, one of them tagged `pilot-mission`, a
passphrase-less fixture key, one real grant). The witness script and its
output are in the molecule's `report.md`. What it showed:

- The tag changes nothing outside `cs sessions seats`. The only code that reads
  it is the seats view; `cs status`, `cs peek` and `cs purge` do not.
- A tagged molecule with no lease is reported exactly like an ordinary one:
  counted as alive and in `backlog.count`, and classified `stalled` by `cs peek` while
  pending.
- Once the mission has a lease ledger, `cs status` counts it as a lease:
  `alive_excluding_leases` drops by one and `backlog.leases_excluded` reads 1,
  so it no longer ages into the stale backlog. This follows from the ledger,
  not from the tag. `cs peek` still lists it.
- With status `running` and no recorded worker, `cs peek --json` reports
  `heartbeat: orphaned` for the tagged and the ordinary molecule alike. `cs
  status` reports no dead worker, because none is recorded.
- `cs purge` removed nothing and changed no molecule status.

Not observed, and therefore not claimed: behavior of a pilot-mission molecule
with a live tmux worker, over the 48-hour stale-backlog window, or under the
scheduler patrols. This issue does not change any sweep.
