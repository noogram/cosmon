# Substrate probe log — what the existing primitives actually do

Run 2026-08-01 against `cs 0.5.0 (8db48169)` — the binary built from the commit
this ADR branches off. Every command below was executed in a throwaway galaxy
under the session scratchpad; none of it touched a real fleet.

Setup:

```console
$ mkdir -p /tmp/m0probe/gal && cd /tmp/m0probe/gal && git init -q . && cs init
```

---

## P1 — `cs presence ping` writes a path `cs diverge` does not read

```console
$ COSMON_SESSION_ID=sess-alpha cs presence ping --headline "primary probe"
presence ping: sess-alpha in cosmon (pid=24634)

$ find .cosmon/state/presence -maxdepth 2
.cosmon/state/presence
.cosmon/state/presence/sess-alpha.json

$ cs diverge sess-alpha sess-alpha
cs: cannot resolve session 'sess-alpha': not a known presence id, not a galaxy directory
$ echo $?
1
```

`PresenceStore` (`crates/cosmon-filestore/src/presence_store.rs`) writes
`presence/<sid>.json`. `cs diverge` (`crates/cosmon-cli/src/cmd/diverge.rs`)
resolves `presence/<sid>/presence.json`. The two agree on the directory and
disagree on everything below it, so session-id resolution in `cs diverge` is
dead for every session `cs presence ping` created. Only the path form works:

```console
$ cs diverge /tmp/m0probe/gal /tmp/m0probe/gal
? INCONCLUSIVE — sessions … vs …
  ? state              no --molecule supplied
  ? current_step       no --molecule supplied
  ? briefing_seals     no --molecule supplied
  ? git_merge_base     one or both cwd are not a git repository
$ echo $?
2
```

## P2 — an unresolvable session is reported as *disagreement*, not *unknown*

`cs diverge` is tri-valued by construction — `Agreement::{Agree,Diverge,Inconclusive}`
map to exit `0/1/2`, and its own unit test `agreement_exit_codes` pins that.
But session resolution fails *before* the verdict is computed, through
`anyhow`, so the caller reads exit `1`:

```console
$ cs diverge /tmp/m0probe/gal /nonexistent-galaxy
cs: cannot resolve session '/nonexistent-galaxy': not a known presence id, not a galaxy directory
$ echo $?
1
```

Exit 1 is the code the module documents as "at least one clause disagrees".
A co-pilot branching on `$?` — which is exactly what the drift comparison in M3
would do — reads *unknown* as *these two pilots disagree*.

## P3 — the session channel is at-most-once, not at-least-once

```console
$ cs whisper --to-session sess-beta -m "msg-one"
cs whisper: target session sess-beta has no presence file — created fresh log at …/presence/sess-beta.log
whispered 7B to session sess-beta
$ cs whisper --to-session sess-beta -m "msg-two"
whispered 7B to session sess-beta

$ cat .cosmon/state/presence/sess-beta.log
2026-08-01T00:03:44.715322+00:00 | from:eserie | msg-one
2026-08-01T00:03:44.755508+00:00 | from:eserie | msg-two

$ cs presence poll --session sess-beta
2026-08-01T00:03:44.715322+00:00 | from:eserie | msg-one
2026-08-01T00:03:44.755508+00:00 | from:eserie | msg-two
$ cs presence poll --session sess-beta        # second poll: nothing
$ cat .cosmon/state/presence/sess-beta.seek
114
```

Read `run_poll` in `crates/cosmon-cli/src/cmd/presence.rs`: the seek pointer is
written **before** the tail is printed. A reader that dies between the seek
write and consuming the text loses those messages permanently. The envelope is
a timestamp, an OS username (`from:eserie` — not a session id) and the text.
There is no message id, no sequence number, no content hash, no read receipt,
no expiry.

## P4 — a rotated log silently swallows the backlog

```console
$ printf 'short\n' > .cosmon/state/presence/sess-beta.log   # rotation / truncation
$ cat .cosmon/state/presence/sess-beta.seek
114
$ cs presence poll --session sess-beta
$ echo $?
0
```

The stale seek (114) is past the new end (6), so `poll` reports success and
prints nothing. The new content is unreachable: `poll` never rewinds, and the
seek is only ever advanced.

## P5 — a stale seek landing mid-codepoint panics the reader

```console
$ python3 -c "open('.cosmon/state/presence/sess-beta.log','wb').write(b'a'*113 + 'é'.encode() + b'\ntail\n')"
$ cs presence poll --session sess-beta
thread 'main' panicked at crates/cosmon-cli/src/cmd/presence.rs:221:40:
byte index 114 is not a char boundary; it is inside 'é' (bytes 113..115) of `aaa…`
$ echo $?
101
```

`&content[seek..]` slices bytes. Any writer that rewrites the log rather than
appending to it — rotation, a hand edit, a compaction — can leave the seek
inside a multi-byte character and take the reading pilot's process down.

---

## P6 — the Claude project directory name is not invertible

`energy_probe::sanitize_path` maps *every* non-alphanumeric byte to `-`:

```rust
path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
```

So `…/cosmon/.worktrees/task-X` and `…/cosmon--worktrees/task-X` produce the
same directory name, and the encoding cannot be decoded back to a path. Repo
identity for a Claude session must be read from the `cwd` field carried inside
every record — visible in Trace A — never from the directory name.

Codex has no equivalent problem: `resolve_codex_session_by_cwd` joins on the
`session_meta.payload.cwd` value itself. It has the mirror-image problem
instead — it returns the *most recently modified* log matching a cwd, so two
Codex sessions in one directory are silently collapsed to one.

## P7 — `claudion::parse_session` cannot read a log that is being written

`parse_session` returns `Err(ClaudionError::JsonParse)` on the first line that
is not complete JSON, and there is no cursor API: the only entry points are
`discover_sessions` (stat) and `parse_session` (whole file). Observing a live
session therefore means re-reading it from byte zero on every poll and failing
outright whenever the sample lands mid-append.
