# Running a claude worker inside a container — operator guide

A cosmon worker is a `claude` process in a tmux pane that **nobody is looking
at**. Every question Claude Code can ask at startup is therefore a place the
worker can stop forever: the pane stays open, the molecule stays `running`, and
the fleet reports a healthy worker that will never produce a token.

This guide is the list of those questions and what cosmon does about each. It
came out of issue #20, reported by an external tester on v0.3.0 running
`cs` directly under an unprivileged uid in a Docker-Desktop-on-macOS arm64
container.

## The four doors that stop an unattended worker

Two of them are blocking dialogs. The third is not a dialog at all, which is the
part everyone gets wrong. The fourth is a dialog again — the onboarding screens
Claude Code shows on a config directory it has never seen — and it was found by
the bench built to measure the third, then re-diagnosed when the first fix for it
was green in every test and red on the bench.

### 1. The folder-trust dialog

```text
 Accessing workspace: /home/cosmon-worker/proj/.worktrees/task-…
 Quick safety check: Is this a project you created or one you trust? …
 ❯ 1. Yes, I trust this folder
   2. No, exit
```

Claude Code asks this in **any directory it has not seen before**, and a fresh
worktree is by definition unseen.

Two things about it are worth knowing, because both are natural wrong guesses:

- **`--permission-mode bypassPermissions` does not suppress it.** Measured
  against Claude Code 2.1.220: with a config whose `projects` map is empty, a
  bypass-mode launch in an unseen directory renders the dialog and waits. Folder
  trust is a property of the *workspace*; no permission mode skips it.
- **Answering it by keystroke is a race.** cosmon's readiness handshake types the
  answer, and on a cold container the `tmux send-keys` can land before the TUI
  has attached its input handler. The keystroke is swallowed and the pane sits on
  the question.

**What cosmon does:** it pre-grants the trust *before spawning*, so the dialog is
never rendered. See `cosmon_transport::claude_trust`.

### 2. The bypass-permissions disclaimer

```text
  WARNING: Claude Code running in Bypass Permissions mode
  ❯ 1. No, exit
    2. Yes, I accept
```

Note the default-highlighted option: a bare Enter **quits the worker**. Also
pre-granted before spawn.

The two pre-grants live in **two different files**, which is the part worth
writing down:

| gate | file (with `CLAUDE_CONFIG_DIR` set) | key |
|---|---|---|
| folder trust | `$CLAUDE_CONFIG_DIR/.claude.json` | `projects["<abs workspace>"].hasTrustDialogAccepted = true` |
| bypass disclaimer | `$CLAUDE_CONFIG_DIR/settings.json` | `skipDangerousModePermissionPrompt = true` |

With `CLAUDE_CONFIG_DIR` unset, Claude Code splits them across directories:
`$HOME/.claude.json` and `$HOME/.claude/settings.json`.

One footprint worth knowing: `skipDangerousModePermissionPrompt` is a *user-scope*
setting, so when `CLAUDE_CONFIG_DIR` points at an account you also use
interactively, you stop seeing the bypass disclaimer in your own sessions. There
is no narrower place to put it, and the cost is nil (it is one-time consent you
have necessarily already given for that account) — but it is a real change to
your settings file, not only to the fleet's.

The `.claude.json` key `bypassPermissionsModeAccepted` **looks** like the second
pre-grant and is not: on 2.1.220 it is a legacy flag kept only for a one-way
migration into settings, and a config carrying it still renders the disclaimer
(measured). Only the settings key works.

### 3. No credential — the quiet one

The tester's report named this door and described it as a login selector. On an
**onboarded** config dir that is not what happens, and the difference is worth
stating precisely, because looking for the wrong shape sends you hunting a dialog
that is not there.

On an onboarded config dir there is **no login selector**. Measured on 2.1.220
with both consent gates pre-granted and a config dir holding no credential, the
TUI boots all the way to the composer:

```text
  ⏵⏵ bypass permissions on (shift+tab to cycle)     Not logged in · Run /login
```

It blocks on nothing. It will accept a pasted briefing and never emit a token,
and every liveness signal cosmon has reports a healthy worker. Of the four
doors this is the quietest failure — which is why it now gets the loudest
refusal.

**But the tester was seeing something real**, one screen earlier in the
corridor: on a *virgin* `CLAUDE_CONFIG_DIR` Claude Code opens on onboarding — the
theme wizard first, the login-method selector behind it — and it blocks there.
That is a different door with a different fix; it is
[door 4](#4-the-onboarding-doors--the-theme-wizard-and-the-login-method-selector)
below.

**What cosmon does:** on the TUI spawn paths it looks for a usable credential
*before spawning*, and refuses the dispatch when there is none:

```text
cs tackle: refusing to spawn a claude worker for molecule task-…: no usable
Claude Code credential for the interactive worker: /home/cosmon-worker/.claude/
.credentials.json does not exist, no keychain item `Claude Code-credentials`
exists, and CLAUDE_CODE_OAUTH_TOKEN is not set (ANTHROPIC_API_KEY is set, but it
is not a TUI credential — it opens its own consent dialog instead). Provision a
credential the interactive worker can use, by either: (a) exporting
CLAUDE_CODE_OAUTH_TOKEN into the dispatcher's environment; (b) running `claude`
once interactively under the same CLAUDE_CONFIG_DIR and completing `/login` …
```

A credentials file that is *present but unreadable by the worker's uid* — the
root-built image dropped to `USER 10001` again — is a separate refusal with a
separate fix (`chown`, not "provision"), because those are different problems.

#### What actually counts as a credential

Four measured arms, each a fresh isolated `CLAUDE_CONFIG_DIR` with both consent
gates pre-granted:

| you provide | Claude Code 2.1.220 does |
|---|---|
| nothing | composer, footer `Not logged in · Run /login` — the mute worker |
| `CLAUDE_CODE_OAUTH_TOKEN` | authenticated, no footer, no dialog ✅ |
| `<config dir>/.credentials.json` | authenticated, no footer, no dialog ✅ |
| `ANTHROPIC_API_KEY` only | **a consent dialog of its own**, default `❯ 2. No (recommended)` |

Three corrections to the folklore, all of them things a naive fix gets wrong:

- **The file is `.credentials.json`, dot-prefixed**, inside the config dir. There
  is no `credentials.json`. A check written from that name would refuse every
  dispatch everywhere.
- **`CLAUDE_CODE_OAUTH_TOKEN` *does* satisfy the TUI.** The report says it does
  not; measurement says it does, and cosmon accepts it. If you are building a
  container, exporting the token is the simplest provisioning that works.
- **`ANTHROPIC_API_KEY` is not a substitute.** It opens its own consent question
  whose default answer is *No* — a brand-new mute hang, not a login. cosmon
  treats it as "no credential" and says so by name in the refusal, because that
  is the belief that costs the time.

#### The one no file check can see

On macOS the credential normally lives in the **login keychain**, not on disk.
The machine cosmon is developed on has no `.credentials.json` anywhere and a
working fleet; a file-presence check alone would have refused every dispatch on
it. The store is keychain-first with the plaintext file as fallback, so the check
probes both. The keychain item is

```text
service = "Claude Code-credentials" + ("-" + sha256(<config dir>)[..8]  if CLAUDE_CONFIG_DIR is set)
account = $USER
```

— derived from the shipped binary and then **confirmed by probing the predicted
name against the real keychain**, which found the live item. The per-config-dir
suffix is what keeps a multi-account fleet's credentials apart. The probe runs
`security find-generic-password` **without `-w`**, so the secret is never even
printed to a pipe; cosmon checks presence and permissions and never reads,
copies, logs, or displays credential content.

#### The layout shift, again

`.credentials.json` follows `settings.json`, **not** `.claude.json`:

| `CLAUDE_CONFIG_DIR` | credentials file |
|---|---|
| set | `$CLAUDE_CONFIG_DIR/.credentials.json` |
| unset | `$HOME/.claude/.credentials.json` |

So it is never `$HOME/.credentials.json`. `CLAUDE_SECURESTORAGE_CONFIG_DIR`
overrides the storage directory when set, and is then the hash input for the
keychain service name too.

#### Which spawn paths refuse, and which do not

The refusal fires on the **interactive** paths: `cs tackle --adapter claude`, and
the `cs thaw` / patrol-respawn path, which re-creates a bare TUI pane and pastes
the resume prompt afterwards. A resumed worker is not a lesser worker.

A **headless** `claude -p` is deliberately exempt. With no credential it *exits*,
non-zero and immediately, which cosmon already classifies through its
adapter-exit path. Door 3 is a doctrine about mute hangs; a process that dies
with a status is not one, and refusing it there would trade a loud failure for
another loud failure while adding a new way to wrongly block a working dispatch.

Diagnostic that still holds: a worker showing the *composer* is past doors 1 and
2; a worker showing a *dialog* never got that far.

### 4. The onboarding doors — the theme wizard and the login-method selector

On a *virgin* `CLAUDE_CONFIG_DIR` (no `hasCompletedOnboarding`), Claude Code
2.1.220 opens onto onboarding and stays there. There are **two** screens, in
this order, and the order is the part that cost the most time to learn:

```text
Welcome to Claude Code v2.1.220          ← first, and the one that blocks a dispatch

 Let's get started.

 Choose the text style that looks best with your terminal

   1. Auto (match terminal)
 ❯ 2. Dark mode ✔
   …
```

```text
Welcome to Claude Code v2.1.220          ← second, only after the wizard is answered

 Select login method:

 ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise
   2. Anthropic Console account · API usage billing
   3. 3rd-party platform · Amazon Bedrock, Microsoft Foundry, or Vertex AI
```

An earlier version of this page said the selector was *first* and that the theme
wizard was a myth. That was wrong, and it was wrong for an instructive reason:
every capture we had was taken **after** `cs tackle` returned, and the briefing
cs typed into the wizard is what answered it and advanced the pane to the
selector. The bench was photographing a screen cosmon's own keystrokes had
summoned. Instrumenting the readiness loop — one trace line per sample, carrying
the bytes classified — is what separated the two.

Both are **onboarding** doors, not credential doors: the container bench isolates
them by running two arms that differ only in `hasCompletedOnboarding`
([`container-worker-doors-bench.md`](container-worker-doors-bench.md)). Door 3's
credential check is satisfied and these screens still appear, because nothing has
told Claude Code how you intend to set up the session.

**What cosmon does — and what it deliberately did not do.** No marker for this
screen was added to `readiness::markers`. Naming it would have shut one door and
left the corridor open, exactly as the three previous names did: every one of
Claude Code's startup screens is a menu, every menu draws `❯` as its selection
cursor, and the old rule — *scan the last five lines for a chevron* — therefore
called each unnamed menu `Ready`. That generic scan is **gone**.

In its place, `Ready` is *earned* by positive evidence that the composer is on
screen, and only two things count as that evidence, both scoped to the frame the
pane is painting right now:

- the placeholder `Type your message` sitting **on** the chevron line — an empty
  input box saying in words that it wants a message; or
- a chevron line that is *not* a menu option, standing in the same tail as the
  composer's own footer (`⏵⏵`, `shift+tab to cycle`, `? for shortcuts`).

A bare chevron is not enough, and neither is a box frame — this TUI boxes its
modals as readily as its composer, so `│ ❯ a) Re-authorise now │` would sail
through a frame-based rule. The selector's chevron rests on `1. Claude account
…`, which is a menu-option *shape*, so it produces no composer evidence and the
pane is classified **`AwaitingHuman`** — along with every other screen nobody has
named yet, including ones that do not exist on this build. That is the point: a
screen this build has never seen cannot become `Ready`, because it cannot show a
composer it is not showing.

`AwaitingHuman` is deliberately still `Live` at the *spawn postcondition* —
something painted a frame, so the binary did run, and a slow cold start is not
torn down for it. The refusal is written one layer up, at the dispatch gate:
`ClaudeTuiProbe::await_live` maps `AwaitingHuman` to `Indeterminate`, and
`cs tackle` is coded to refuse there, quote the pane before tearing it down, and
exit non-zero:

```text
cs tackle: claude session cosmon-task-… never reached a work-accepting state
within 30s (status=unknown). The pane is alive but is not a composer — typically
an onboarding or consent screen waiting for a human (run `claude` once in this
CLAUDE_CONFIG_DIR to answer it), or a binary that started and printed nothing
recognisable. Pane showed: Select login method: | ❯ 1. Claude account with
subscription · Pro, Max, Team, or Enterprise | … Inspect with `tmux -L <socket>
capture-pane -pS - -t <session>` then retry with --force …
```

**The composer rule was necessary and it was not sufficient.** It kept the
*selector* out of `Ready`, and the container kept dispatching anyway, because the
screen that decided the dispatch was the theme wizard one step earlier — and the
wizard is `Loading`, a status the composer rule was never asked about. The gate
below it collapsed `Loading` to `Live`, so the briefing went out.

**The second half: the dispatch gate is an allow-list.** Only `Ready` and
`Working` mean *"accepting work"*; `Dead` means dead; **everything else is
refused**, including `Loading`. The rule is one sentence: `wait_ready` returns
`Ready` / `Working` the moment it sees them and returns everything else only by
exhausting its window, so a status that arrives by timeout is not evidence of
anything. Naming the wizard would have been the fifth door named and the fifth
corridor left open.

`cs tackle` refuses there, quotes the pane before tearing it down — head and
tail, because these screens carry their identity in the headline and their
mechanics at the foot — and exits non-zero:

```text
cs tackle: claude session probe-… never reached a work-accepting state within
30s (status=loading). The pane is alive but is not a composer — typically an
onboarding or consent screen waiting for a human (run `claude` once in this
CLAUDE_CONFIG_DIR to answer it), or a binary that started and printed nothing
recognisable. Pane showed: Welcome to Claude Code v2.1.220 | Let's get started.
| … | Syntax theme: Monokai Extended (ctrl+t to disable). Inspect with `tmux -L
<socket> capture-pane -pS - -t <session>` then retry with --force …
```

**Proven in a container, not only in a test.** On the run of 2026-07-25 the
bench's arm C — virgin config dir, `desktop-linux`, Claude Code 2.1.220, no real
credential anywhere — showed all four post-conditions of a correct refusal:
`cs tackle` exits 1, its stderr quotes the screen it refused, the tmux session is
gone, and the molecule is back to `pending` rather than parked `running` behind a
blocked pane. The spawn postcondition still passes a slow cold start, which is
the price this fix was not allowed to charge.

**What the operator still does, and why it is still the better gesture:** seed
the config directory once and let every container reuse it — run `claude` a
single time under the same `CLAUDE_CONFIG_DIR`, answer both onboarding screens,
and the `hasCompletedOnboarding` key it writes is what later spawns inherit.
Baking an onboarded config into the *image* is the same idea with a coarser
grain. cosmon now refuses loudly instead of hanging mutely, but a refusal is
still a dispatch that did not happen; the seeded config is what makes the
dispatch work.

## Ownership: `--add-dir` is authorization, not ownership

A worker's cwd is its worktree, but the molecule state, fleet lock, and
`events.jsonl` it writes on `cs evolve` / `cs complete` live in the **main**
repo's out-of-worktree `.cosmon/`. cosmon declares that directory writable with
Claude Code's `--add-dir`, on every spawn path and for every permission mode.

`--add-dir` grants *Claude Code* permission to attempt the write. It grants the
*process* nothing. The classic container shape breaks on exactly this:

```dockerfile
# image built as root …
COPY . /home/cosmon-worker/proj
# … then dropped to an unprivileged uid
USER 10001
```

`/home/cosmon-worker/proj/.cosmon` stays root-owned, the worker is granted it,
starts, is declared live, and fails `EACCES` the first time it writes molecule
state.

cosmon now refuses that dispatch up front, naming the uid and the path:

```text
cs tackle: refusing to spawn a claude worker for molecule task-…: uid 10001
cannot use the StateDir it must write: /home/cosmon-worker/proj/.cosmon.
`--add-dir` grants Claude Code authorization, not filesystem ownership …
```

The fix is on the image side:

```dockerfile
RUN chown -R 10001:10001 /home/cosmon-worker/proj
```

## The rule behind all of it: fail loud, never mute

Every check above refuses **before** a live worker exists, and says why. That
asymmetry is deliberate. A dispatch that fails with a reason costs the operator
one message; a worker that hangs mutely on a dialog holds a molecule slot,
reports healthy, and is found hours later. A mute hang is worse than a stated
refusal — so when consent cannot be pre-granted or a granted path is not
reachable, cosmon declines the spawn rather than hoping.

## Verifying by hand

The pre-grant is exercised by unit tests
(`cosmon_transport::claude_trust::tests`), but whether Claude Code still honours
those two keys is a property of the *installed binary*, which no hermetic test
can pin. Re-verify after a Claude Code upgrade:

```sh
CFG=$(mktemp -d); WS=$(mktemp -d)
# 1. RED — no pre-grant: the trust dialog appears even in bypass mode.
printf '{"hasCompletedOnboarding":true,"projects":{}}' > "$CFG/.claude.json"
tmux -L trustcheck new-session -d -c "$WS" \
  "CLAUDE_CONFIG_DIR=$CFG claude --permission-mode bypassPermissions"
sleep 12; tmux -L trustcheck capture-pane -p | tail -20   # expect the dialog
tmux -L trustcheck kill-server

# 2. GREEN — both gates pre-granted: straight to the composer, no dialog.
python3 - "$CFG" "$WS" <<'PY'
import json, sys
cfg, ws = sys.argv[1], sys.argv[2]
json.dump({"hasCompletedOnboarding": True,
           "projects": {ws: {"hasTrustDialogAccepted": True}}},
          open(f"{cfg}/.claude.json", "w"))
json.dump({"skipDangerousModePermissionPrompt": True},
          open(f"{cfg}/settings.json", "w"))
PY
tmux -L trustcheck new-session -d -c "$WS" \
  "CLAUDE_CONFIG_DIR=$CFG claude --permission-mode bypassPermissions"
sleep 12; tmux -L trustcheck capture-pane -p | tail -8    # expect the composer
tmux -L trustcheck kill-server
```

The credential question is verified the same way — by *watching the footer*, which
is the only place door 3 announces itself. This is the method to re-run after a
Claude Code upgrade, and the one used to establish the table above:

```sh
# Fresh, ISOLATED config dir: the keychain service name is derived from the
# config dir path, so a new directory means a new (absent) keychain item. That
# is what makes this arm credential-free even on macOS.
CFG=$(cd "$(mktemp -d)" && pwd -P); WS=$(cd "$(mktemp -d)" && pwd -P)
python3 - "$CFG" "$WS" <<'PY'
import json, sys
cfg, ws = sys.argv[1], sys.argv[2]
json.dump({"hasCompletedOnboarding": True,
           "projects": {ws: {"hasTrustDialogAccepted": True}}},
          open(f"{cfg}/.claude.json", "w"))
json.dump({"skipDangerousModePermissionPrompt": True},
          open(f"{cfg}/settings.json", "w"))
PY
# `pwd -P` matters: claude canonicalizes the workspace, and on macOS
# /var/folders/… is a symlink to /private/var/folders/… — a trust key written
# against the uncanonicalized path silently fails to match and you get door 1
# instead of the arm you meant to measure.

tmux -L logincheck new-session -d -x 200 -y 50 -c "$WS" \
  "CLAUDE_CONFIG_DIR=$CFG claude --permission-mode bypassPermissions"
sleep 22; tmux -L logincheck capture-pane -p | tail -3   # expect: Not logged in · Run /login
tmux -L logincheck kill-server

# Then repeat with, in turn:
#   CLAUDE_CODE_OAUTH_TOKEN=…   → expect NO footer (authenticated)
#   a $CFG/.credentials.json    → expect NO footer (authenticated)
#   ANTHROPIC_API_KEY=…         → expect a consent dialog, default "No"
```

If the third arm starts showing the footer, the credentials file moved and
`cosmon_transport::claude_login` needs updating; the module docs record the same
method plus the keychain derivation, so re-run it there.

If step 2 shows a dialog, Claude Code moved the keys and
`cosmon_transport::claude_trust` needs updating — the module docs record how the
current key names were established (decompiled gate plus this experiment), so the
same method applies.

Onboarding note: a genuinely fresh `CLAUDE_CONFIG_DIR` renders onboarding, which
is why the snippet seeds `hasCompletedOnboarding`. cosmon does not pre-grant
onboarding, and the two onboarding screens are classified differently — but
neither is dispatched to:

| screen | marker | classified | dispatch |
|---|---|---|---|
| theme wizard (`Choose the text style`) | `FIRST_RUN_THEME` | `Loading` — a cold start still in progress | the handshake waits |
| login-method selector (`Select login method`) | *none, by design* | `AwaitingHuman` — no composer evidence | refusal coded; **not yet observed live** |

Naming the wizard says "still booting, keep waiting"; the unnamed class says "a
human is needed here". The second column is what the classifier returns, and it
is measured. The fourth is what the dispatch gate is written to do with it, and
on the container bench it has not yet been seen to happen — so seed the config
dir rather than relying on it. See door 4 above.
