<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Event-driven briefing receipt — prototype and measurement harness

The question, in one sentence: cosmon currently learns that a briefing was
submitted by pausing 500 ms and then reading the composer every 300 ms until
the pasted text stops being there — can Claude Code instead *tell* it, and is
the answer it gives better than the guess?

This is the follow-up `experiments/briefing-submit-race` opened and explicitly
did not test. Nothing here changes production behaviour: the 500 ms pause, the
retry timing and the composer check in `cosmon-transport/src/tmux.rs` are
untouched, and the hook is not installed anywhere outside a scratch directory.

## The mechanism under test

An ephemeral settings file, minted per session in a scratch directory and
handed to the process with `claude --settings <file>`, registers one
`UserPromptSubmit` hook. Cosmon stamps a per-dispatch nonce, pastes, presses
the carriage return **immediately**, and then waits for the hook to write a
receipt keyed to that nonce — re-pressing only while no receipt exists.

`--settings` is additive and file-scoped: the overlay is a new file, and the
user, project, local and managed settings are neither read nor written by
anything in this directory.

## What a receipt proves, and what it does not

`UserPromptSubmit` fires when a prompt **enters Claude Code's lifecycle**. It
does not mean the model started producing tokens: a second hook on the same
event can still exit 2 and block the prompt after ours has already written its
receipt. The `blocking_hook` scenario measures exactly that, and it is the
reason `Receipt.evidence` is a variant and not a boolean — see `receipt.py`.

So the receipt retires "did the submit land?" and leaves "is the worker
working?" to the `Working` / `⏺` acceptance signal, unchanged.

## Files

| file | what it is |
|---|---|
| `ack_hook.py` | the hook itself: no stdout, no prompt content, atomic write, always exit 0 |
| `receipt.py` | the prototype submit path and the typed `Receipt` outcome |
| `hookmatrix.py` | the trial runner: two arms × the scenario grid, on an isolated tmux socket |
| `aggregate.py` | trial lines → the tables in the write-up |
| `ptyspy.py` | PTY interposer (copied from `experiments/briefing-submit-race`) — counts the carriage returns the *application* received |

## Running it

```sh
python3 hookmatrix.py --workdir <scratch> --ws <trusted-cwd> --out results.jsonl \
    --arms prod,event --scenarios normal,tuned,busy --sizes 12,100 --reps 4
python3 aggregate.py results.jsonl
```

`--ws` must be a directory Claude Code already trusts, or every trial stalls on
the folder-trust dialog. The tmux socket must live in the `cosmon-test-`
namespace — the runner refuses anything else — and is killed on every exit
path including a signal, so the fleet socket is never touched.

## The scenarios, and why each is there

| scenario | what it forces |
|---|---|
| `normal`, `busy` | the two conditions the race harness established; `busy` is a nudge into a TUI that is already thinking |
| `no_retry` | one carriage return only, so the receipt's arrival time measures the hook's own latency and cannot be confounded by a retry that happened to land first |
| `tuned`, `tuned_busy` | the retry interval raised above the measured hook latency — the configuration actually being recommended |
| `user_hook` | an operator hook already registered on `UserPromptSubmit`; ours must coexist rather than replace, and must not be delayed past usefulness by theirs |
| `blocking_hook` | a second hook that exits 2. Decides what a receipt is allowed to claim |
| `hook_fail` | the hook command cannot execute — configured but broken |
| `hooks_disabled` | no overlay at all: hooks off, refused by managed policy, or an older build |
| `unwritable_ack` | receipt directory mode 0500 |
| `malformed_dest` | receipt destination is a regular file, not a directory |
| `stdout_leak` | a hook that deliberately writes to stdout, to measure what leaked stdout does |
| `suppress_first_cr` | the first carriage return is never sent, so only the receipt-driven retry can recover the submit |
| `two_dispatches` | two briefings in one session, to show the nonce rotating and the second receipt not matching the first |

The `prod` arm runs only under `normal` and `busy`: it has no hook, so the hook
failure modes have nothing to say about it.
