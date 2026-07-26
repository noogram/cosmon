# Which pre-granted keys survive a Claude Code lifecycle — 2.1.220

*Measured 2026-07-26. Follow-up to noogram/cosmon#20, on `@jdthaler`'s report
that the installer now ships 2.1.220, that it opens on a syntax-theme wizard, and
that a pre-granted `hasCompletedOnboarding` was gone by the next spawn.*

The rule this exists to honour: **do not infer from documentation or from our own
2.1.218 notes.** The lesson of that report is precisely that a version moved
under a supposition. Everything below is a reading taken on the installed binary,
with the method written out so it can be re-run after the next upgrade.

## Bench

| | |
|---|---|
| host | macOS 15 (Darwin 25.5.0), arm64 |
| Claude Code | `2.1.220 (Claude Code)`, native install |
| driver | `tmux 3.5a`, detached session, 200×50 |
| launch | `CLAUDE_CONFIG_DIR=<cfg> claude --permission-mode bypassPermissions` |
| workspace | a fresh empty directory, `pwd -P` canonicalized |
| credential | **none** — a pristine config dir is unauthenticated by construction, since Claude Code derives its keychain service name from the config-dir path |

Method for every run: seed `<cfg>`, snapshot both files, launch, capture the pane
every 5 s, snapshot again, quit with two `C-c`, wait, snapshot a third time. The
third snapshot is the one that answers the durability question, because the
rewrite happens when the session *ends*.

## Run A — pristine config dir, nothing seeded

The screen the tester reported, reproduced verbatim:

```text
Welcome to Claude Code v2.1.220

 Let's get started.

 Choose the text style that looks best with your terminal
 To change this later, run /theme

   1. Auto (match terminal)
 ❯ 2. Dark mode ✔
   …
 Syntax theme: Monokai Extended (ctrl+t to disable)
```

It is still on that screen at 25 s and after the quit. `<cfg>/.claude.json` was
**never created** — Claude Code does not persist anything until onboarding
completes. `readiness::classify_output` calls this pane `Loading`, the dispatch
gate refuses it, `cs tackle` exits non-zero quoting the pane. That is the fix
from `b0995d3` behaving as designed, and it is also a molecule that did not get
worked.

## Run B — four keys seeded, one graceful lifecycle

Seeded into `.claude.json`: `hasCompletedOnboarding`, `theme`,
`bypassPermissionsModeAccepted`, `projects[ws].hasTrustDialogAccepted`. Into
`settings.json`: `skipDangerousModePermissionPrompt`, `theme`.

The pane reached the composer at 5 s — `⏵⏵ bypass permissions on (shift+tab to
cycle)` with the `Not logged in · Run /login` footer. No wizard, no trust dialog,
no disclaimer.

After a graceful quit:

| key in `.claude.json` | before | after |
|---|---|---|
| `hasCompletedOnboarding` | `true` | **`true`** |
| `projects[ws].hasTrustDialogAccepted` | `true` | **`true`** |
| `theme` | `"dark"` | **absent** |
| `bypassPermissionsModeAccepted` | `true` | **absent** |

`settings.json` was byte-identical — Claude Code never wrote it.

`.claude.json` gained 14 keys it wrote itself (`numStartups`, `machineID`,
`migrationVersion`, `lastReleaseNotesSeen: "2.1.220"`, per-project
`lastSessionId` / timing metrics, …). **So the file is not edited in place: it is
rewritten from the process's in-memory state, and keys the running build does not
recognise are dropped.** `theme` and `bypassPermissionsModeAccepted` are exactly
that — both were meaningful to an earlier build.

This is the mechanism the tester hit. On his bench the key that got dropped was
`hasCompletedOnboarding`; on this one it survived. Same rewrite, different
survivor set. **Which keys survive is not something cosmon can know**, and that,
not the particular outcome, is the finding.

## Run C — is the write lost when a live process is holding the file?

The competing hypothesis: cosmon writes the key while an earlier worker is still
running, and that worker's exit-time rewrite clobbers it (a lost update).

Method: launch with the key present, wait 15 s for the composer, then write a
*second* workspace's trust key into `.claude.json` from outside, then quit the
running process.

Result — the mid-flight write **survived** the process's exit:

```text
mid-flight write done:   { "onb": true, "ws2": true }
after graceful quit:     { "onb": true, "ws1": true, "ws2": true }
```

So Claude Code re-reads the file before rewriting it. Concurrent pre-grants are
not lost. **Hypothesis refuted** — the erasure is key-stripping, not a race.

## Run D — is `settings.json` a durable home for the onboarding key?

`settings.json` is never rewritten, which makes it the obvious candidate. Seeded
`hasCompletedOnboarding: true` into `settings.json` and *not* into
`.claude.json`.

Result: **the theme wizard rendered.** The key is not honoured there. Verified in
the same run that `settings.json` still held it afterwards, untouched — it is
durable and ignored, which is the worst of both.

The other two candidates were checked and are also closed:

- `claude config set -g hasCompletedOnboarding true` → `error: unknown option '-g'`;
- no environment variable is exposed for onboarding state.

**There is no durable channel.**

## Run E — does a hard kill corrupt or regress the grant?

A container teardown is a `SIGKILL`, not a graceful quit. Launched with the key,
reached the composer, `kill -9`.

Result: `hasCompletedOnboarding` and the trust key both still `true`; no
truncated file. The zero-byte case `claude_trust::read_json_object` already
tolerates was not produced here, but the guard stays — it costs nothing and a
killed process mid-write is exactly what it is for.

## Verdict — re-assert before every spawn

Voie (a) of the report, in its "re-apply before each spawn" form, and it is the
only one the measurements leave open:

- there is no durable location (run D, plus the two closed CLI/env candidates);
- `.claude.json` may silently lose any key at any version bump (run B);
- but a write is never lost to a concurrent process (run C) and survives a hard
  kill (run E), so re-asserting is cheap and reliable.

`pregrant_startup_consent` already ran before every spawn and read-modify-wrote
both files; it was missing one key. It now asserts `hasCompletedOnboarding` too.
Nothing may hoist this into a run-once install step — the module docs say so, and
two tests enforce it.

Voie (b) — teaching the readiness loop to answer the wizard — was **rejected**.
§8v (ADR-162) says a screen cosmon cannot certify is refused and that cosmon does
not drift into piloting onboarding. Pre-granting in a config file does not cross
that line: the classifier is unchanged, no marker was added, a wizard that
renders anyway is still refused with the pane quoted, and the wizard is not
*answered* — it is not *rendered*. Answering it would also fail on its own terms,
since every startup screen this TUI paints is a menu, so naming the fifth door
leaves the sixth open. That is the corridor issue #20 walked through four times.

## Acceptance — two consecutive dispatches, nothing in between

One green dispatch cannot distinguish a re-asserted grant from a run-once one.
The criterion is two, on a pristine config dir, with no human between them:

```console
$ cargo test -p cosmon-transport --test claude_consent_live -- --ignored --nocapture
claude version: 2.1.220 (Claude Code)
spawn-1: pre-grant outcome = Granted
after spawn 1: hasCompletedOnboarding=true projects[ws].hasTrustDialogAccepted=true
spawn-2: pre-grant outcome = AlreadyGranted
after spawn 2: hasCompletedOnboarding=true projects[ws].hasTrustDialogAccepted=true
spawn 1: reached the composer, no dialog
spawn 2: reached the composer, no dialog
test two_consecutive_spawns_on_a_pristine_config_dir_never_meet_a_dialog ... ok
```

Run twice, 52 s each. Spawn 2 reports `AlreadyGranted` **on this build**, because
here the keys happened to survive; on a build that strips them it reports
`Granted` and rewrites. Both paths reach the composer, which is the point — the
pre-grant does not depend on knowing which build it is on.

The container-side counterpart is arm F of
[`container-worker-doors-bench.md`](../guides/container-worker-doors-bench.md),
which does the same thing through two real `cs tackle` dispatches.

## Re-run this after the next Claude Code upgrade

The live test is the fast check. When it goes red, the runs above are the method
for finding the new key names: seed candidates, launch, snapshot before/during/
after, and diff. Record the result here rather than in a commit message — the
whole cost of this round was a note that said 2.1.218 and a binary that said
2.1.220.
