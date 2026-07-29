# Run and pilot a real cosmon mission in a container — end-to-end

You have a laptop, Docker, and a Claude Code subscription. At the end of this
page you will have a container with its own little cosmon workshop inside it,
you will have handed that workshop a job, and you will have watched an agent do
the job with your own eyes.

Beyond Docker itself, nothing is installed on your laptop — no `cs`, no
`claude`, no galaxy. All of it lives inside the container, and when you delete
the container it goes with it.

This page is a walkthrough. The page next door,
[Running a claude worker inside a container](claude-worker-in-a-container.md),
is the reference: it explains *why* each gate exists and what it refuses. Read
this one to get something running; read that one when something refuses and you
want to know what it saw. They do not repeat each other.

## What is verified here, and what is not

Every command below was run before it was written down, on 2026-07-27, on the
engine described in the next section. That is not a formality — a previous
version of this harness printed an instruction (`docker start -ai`) that was
plausible, committed, and wrong, because nobody executed it.

Two things on this page were **not** re-run while writing it, and they are
flagged again where they appear:

- **The live worker itself.** Doing that requires putting a real credential in
  a container, and the session that wrote this page is not allowed to touch
  one. What is written about the live dispatch comes from the external tester's
  own measurement on issue #20, quoted where it is used.
- **The glyph half of the locale problem.** The locale *state* below was
  measured here; the rendering differential it causes was measured on
  2026-07-27 and lives in `crates/cosmon-transport/src/locale.rs`.

And one thing nobody has done yet, anywhere: **a long mission**. See
[What nobody has proven yet](#what-nobody-has-proven-yet) before you plan around
this.

## The picture: the container is its own workshop

This is the one idea that makes everything else obvious, so it goes first.

A cosmon galaxy is a directory with a `.cosmon/` folder in it. That folder is
the whole fleet: the molecules, the state, the ledger. There is no daemon, no
server, no shared registry. Whichever `.cosmon/` you are standing next to *is*
the fleet you are talking to.

So when you make a galaxy inside a container, that container has **its own
fleet**, and it has nothing to do with the fleet on your laptop. Same binary,
same commands, two entirely separate workshops that cannot see each other.

Measured, both halves at the same instant — the same command, one inside, one
outside:

```console
$ docker exec -w /srv/mission cosmon-tutorial-02dc cs peek --once --no-tui
[18:05:35] BASELINE 0 worker(s), 1 molecule(s)
[18:05:35] + molecule task-20260727-5909  status: ○ pending  step: 0/2  worker: (none)

$ cs peek --once --no-tui          # …on the host, same second
[18:05:35] BASELINE 39 worker(s), 305 molecule(s)
[18:05:35] + worker adversarial-re-review-8152  status: active  desired: running
…
```

One molecule in there. Three hundred and five out here. Neither knows about the
other.

The consequence you will hit within ten minutes: **you cannot watch the
container's worker from your laptop.** `cs peek` on the host will never show it.
Watching means opening a second door into the same container. That is what the
[watching step](#step-7--watch-the-worker-work) is about.

## Before you start — the engine

These instructions were run on **Colima**, which is also the engine the external
tester who found most of these problems runs on: Lima-based, Ubuntu 24.04.4 LTS,
aarch64.

That is not a preference. Colima and Docker Desktop behave *differently* in two
ways that matter here, and both were measured rather than assumed
([the capture](../benches/engine-fidelity-2026-07-27.md)): on Colima a
bind-mounted host directory silently ignores `chown`, and `unshare` is blocked
by the default seccomp profile. On Docker Desktop neither happens — which sounds
better and is worse, because it means Docker Desktop cannot *show* you a whole
class of failure that will still bite you elsewhere.

cosmon's own benches decide their engine in exactly one file,
[`scripts/lib/bench-engine.sh`](../../scripts/lib/bench-engine.sh), and they
refuse to run rather than quietly fall back to some other context. Use the same
convention here instead of inventing a second one:

```sh
colima start --profile cosmon-bench \
  --cpu 4 --memory 8 --disk 60 --vm-type vz --mount-type virtiofs --runtime docker

export DOCKER_CONTEXT=colima-cosmon-bench
docker info --format 'server={{.ServerVersion}} os={{.OperatingSystem}} kernel={{.KernelVersion}}'
```

Every command from here on assumes `DOCKER_CONTEXT` is exported, so no line
below carries a `--context` flag.

If you already have Colima running for something else, give this its own
profile anyway. A profile shared with real work is a profile that will one day
eat the real work.

## Step 1 — build the image

The image in `docker/container-real-mission/` is the tester's environment,
transcribed: Debian, tmux, git, `claude`, and a `cs` compiled from this
worktree. It contains **no credential of any kind**, deliberately.

Substitute your own tag. Everything else on this page is copy-pasteable as-is.

```sh
docker build -f docker/container-real-mission/Dockerfile \
             -t cosmon-tutorial-02dc:local .
```

It compiles `cs` in release mode, so the first build takes a few minutes.

```console
$ docker run --rm --entrypoint sh cosmon-tutorial-02dc:local -c \
    'cs --version; claude --version; git --version; tmux -V'
cs 0.3.0
2.1.220 (Claude Code)
git version 2.39.5
tmux 3.3a
```

## Step 2 — one container, and a name nobody else will claim

Pick a container name that belongs to this piece of work and to nothing else.
Not `cosmon`, not `mission`, not `worker`.

This is not tidiness. On 2026-07-27 an automated worker followed a brief that
named a shared container, reused it, and destroyed a live twenty-minute
mission — its molecule, its worktree, its compiled artifacts, and the operator's
login inside it. **A hardcoded container name is a shared resource, exactly like
a shared file.** Put something in yours that nothing else will guess: a ticket
id, a date, a random suffix.

Now start it, and notice the entrypoint:

```sh
docker run -dit --init --name cosmon-tutorial-02dc \
  --entrypoint sleep cosmon-tutorial-02dc:local infinity
```

`sleep infinity` is the entire job of this container. It sits there. Everything
you do next is a `docker exec` *into* it — the login, the mission, the watching.
The container is a room you keep unlocked, not a command you keep re-running.

`--init` gives it a real pid 1, which matters because tmux and `claude` leave
children behind that somebody has to reap.

> **Do not use `docker start` to run a second thing.** It is the obvious-looking
> move and it does not do what it looks like. `docker start` replays the
> entrypoint the container was **created** with — measured, on a throwaway:
>
> ```console
> $ docker run --name probe-02dc --entrypoint echo alpine:3 "ENTRYPOINT-RAN"
> ENTRYPOINT-RAN
> $ docker start -a probe-02dc
> ENTRYPOINT-RAN
> ```
>
> A container created to log you in will hand you the login screen forever.
> `docker exec` is the only way to run a *different* act inside one container.

## Step 3 — the credential

A cosmon worker is a `claude` process in a pane nobody is watching. If it has no
credential it stops and waits, and a waiting worker looks exactly like a
thinking worker. cosmon therefore refuses to spawn one at all — measured, in the
container we just started:

```console
$ docker exec -e CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-mission \
    -w /srv/mission cosmon-tutorial-02dc cs tackle task-… --adapter claude
cs: cs tackle: refusing to spawn a claude worker for molecule task-…:
no usable Claude Code credential for the interactive worker:
/home/cosmon-worker/.claude-mission/.credentials.json does not exist,
no keychain item `Claude Code-credentials-ed4fd8c7` exists, and
CLAUDE_CODE_OAUTH_TOKEN is not set. …
```

That refusal is the software working. It leaves you holding a choice it cannot
make for you.

### The recommended route: a token in the environment

Once, on **your own machine** — not in the container:

```sh
claude setup-token          # opens a browser; prints a token. Copy it.
```

Then hand it to the container as an environment variable:

```sh
export CLAUDE_CODE_OAUTH_TOKEN='sk-ant-oat01-PLACEHOLDER-NOT-A-REAL-TOKEN'
```

That string is a placeholder and nothing else — it will not authenticate
anything. Put your own token there and keep it out of your shell history, out of
scripts, and out of git.

This works from a completely virgin config directory: no interactive login
inside the container, no `claude -p` warm-up, nothing seeded. The external
tester confirmed it on his own bench on issue #20, after the ownership fix
landed — two consecutive dispatches, nothing touched between them, each spawning
a live worker that did real work and committed a real artifact:

```text
config dir at start: []

=== DISPATCH 1 ===   artifact: [alpha]   config: {"onboard":true,"projects":2}
=== DISPATCH 2 ===   artifact: [beta]    config: {"onboard":true,"projects":3}
    (no reseed, no intervention)
```

The second dispatch is the one that matters. A setup that passes once and fails
the second time is the trap this whole arc was about.

**Say plainly what that token is.** `claude setup-token` needs a subscription,
and it mints against the account you are logged into. A worker in that container
consumes *that account's* quota, shared with your own sessions. A long-lived
token sitting in a container is **account access, not session access** — if the
container leaks it, rotating the token at the source is the only real remedy;
deleting the container is not.

And on the "at least there is no file on disk" comfort: there is one fewer file,
and that is genuinely something. But an environment variable has its own
exposure surfaces — it shows in process listings, it is readable through
`/proc`, every child process inherits it, and it lands in core dumps. Fewer
files is not the same as strictly safer, and it would be dishonest to write it
that way.

### The fallback: log in by hand inside the container

Still perfectly valid, and the right choice when you would rather no token
existed outside a browser at all:

```sh
docker exec -it -e CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-mission \
  cosmon-tutorial-02dc /usr/local/bin/container-real-mission-login
```

Complete `/login` in the TUI with your own hands, then quit it. The credential
is born inside the container and dies with `docker rm`. Nothing on your host
ever holds it, and no automation is on the path between you and it.

The cost is that it does not automate, by construction: one human gesture per
container, and it needs a real terminal. Ten fresh containers are ten logins.

A third route — bind-mounting your host's `.credentials.json` in — exists, and
its costs are laid out in
[the reference guide](claude-worker-in-a-container.md#b-mount-an-existing-host-credential-into-the-container).
Read that before choosing it.

## Step 4 — three ways to pilot, and the one to pick

Now the question this page exists for: once the container is running, how do you
actually drive it?

**Shape 1 — stay on the host, prefix every command.**

```sh
docker exec -e LC_ALL=C.UTF-8 -e CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-mission \
  -w /srv/mission cosmon-tutorial-02dc cs peek --once --no-tui
```

It works. It is also five things to get right before you have typed the word
`cs`, and both of those environment variables fail in ways that do not look like
missing environment variables (see the two traps below). You will forget one.

**Shape 2 — open the door once and stand inside. Recommended.**

```sh
docker exec -it -u 10001:10001 \
  -e HOME=/home/cosmon-worker \
  -e CLAUDE_CODE_OAUTH_TOKEN="$CLAUDE_CODE_OAUTH_TOKEN" \
  -e CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-mission \
  -e LC_ALL=C.UTF-8 \
  -w /srv/mission cosmon-tutorial-02dc bash
```

You get a shell prompt inside the container, with the environment set once, and
from there every cosmon command is just itself: `cs nucleate`, `cs tackle`,
`cs peek`, `cs done`. Fewest moving parts, fewest things to forget. If you like,
run a Claude Code session in that shell too and let it co-pilot — it is the same
room your workers are in.

Two things about that command are load-bearing, and both are there because
leaving them out is what four separate bugs looked like.

**`-u 10001:10001`, and no `COSMON_WORKER_UID`.** You are the same user your
workers are. Nothing is created by one identity and handed to another, so there
is nothing to hand over and nothing to get wrong. `docker exec` without `-u`
gives you root — not because anyone chose root, but because that is what the
flag's absence means — and root then has to give every new file away to the
worker afterwards. That give-away was found incomplete three times and too
generous once, in two days. The fix is not a better give-away. See
[ADR-165](../adr/165-resources-are-created-under-the-identity-that-consumes-them.md).

**`-e HOME=/home/cosmon-worker`.** Changing only `-u` and leaving `HOME` alone
leaves it pointing at `/root`, which is mode 0700 and belongs to root. Your
shell starts fine and then cannot read its own config directory. That is not a
hypothetical; it is one of the four. If you copy half this command, you get
that bug back.

Measured, with a real terminal:

```console
$ docker exec -it -u 10001 -w /srv/mission -e LC_ALL=C.UTF-8 \
    cosmon-tutorial-02dc bash -lc 'tty; cs observe task-20260727-5909'
/dev/pts/1
Molecule: task-20260727-5909
  Status:          pending
  Formula:         task-work
  Step:            0/2
  Variables:
    topic = add a one-line usage example to README.md
```

**Shape 3 — `cosmon-remote` over the RPP.** The proper front door: a typed API,
`--json` on everything, an audit envelope per request. It needs an OIDC identity
provider and TLS in front of it, which is a lot of machinery for one person with
one laptop.

Could it be made lighter — say, trusting anything that reaches a loopback port?
That was asked properly, as a deliberation
(`delib-20260727-f9ee`), and answered **five seats to zero: no**. The finding
worth carrying here is the shape of the failure, not the vote. `docker exec`
requires you to be root or in the `docker` group; when you are not, it fails
**closed, loudly, at the moment you misuse it**. A loopback door fails **open,
silently, at container start**, and you find out much later or never. Do not
re-argue this; read the deliberation if you want the reasoning.

### Trap 1 — `LC_ALL` on every exec, including the one you watch with

The container's default locale is POSIX. Measured:

```console
$ docker exec cosmon-tutorial-02dc locale | head -3
LANG=
LANGUAGE=
LC_CTYPE="POSIX"

$ docker exec -e LC_ALL=C.UTF-8 cosmon-tutorial-02dc locale | head -3
LANG=
LANGUAGE=
LC_CTYPE="C.UTF-8"
```

Under POSIX, tmux believes your terminal cannot draw non-ASCII and replaces
every such character with `_` — not just the box-drawing of the worker's
interface, but the *text*: `Cramér` arrives as `Cram_r`. (That rendering
differential was measured on 2026-07-27 and is recorded in
`crates/cosmon-transport/src/locale.rs`; the locale states above are what was
re-measured here.)

cosmon fixes the half it owns. Every tmux pane it spawns now gets a UTF-8 locale
floor, and the attach line `cs tackle` prints carries the locale with it, so a
line you copy from cosmon works. What cosmon **cannot** fix is a client it never
spawned — and that is you, attaching by hand. The decision is taken per
attaching client, so no amount of locale on the server side helps.

So: `LC_ALL=C.UTF-8` on every exec you type yourself. The `bash` one. The
`cs peek` one. The attach one.

### Trap 2 — `CLAUDE_CONFIG_DIR` does not survive between execs

It is a property of *one exec*, not of the container. Measured:

```console
$ docker exec -e CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-mission \
    cosmon-tutorial-02dc sh -c 'echo "[${CLAUDE_CONFIG_DIR:-<unset>}]"'
[/home/cosmon-worker/.claude-mission]

$ docker exec cosmon-tutorial-02dc sh -c 'echo "[${CLAUDE_CONFIG_DIR:-<unset>}]"'
[<unset>]
```

Omit it on a later exec and the credential gate looks in
`/home/cosmon-worker/.claude/` instead, finds nothing, and refuses — while your
credential sits intact a few centimetres away in the other directory. The
refusal message names a path; when it names a path you did not expect, this is
why.

Carry it on every exec, or bake it into the image with `ENV`. Shape 2 above
solves this by setting it once, which is most of why it is the recommendation.

## Step 5 — make a galaxy inside the container

From here on, assume you are **inside** the container (shape 2), so the commands
are bare.

One rule about where the project lives:

**On the container's own filesystem, never a `-v` bind mount from macOS.** On a
virtiofs mount `chown` returns success and changes nothing, so an ownership
check can fail on a path you can prove you fixed. Measured on this engine; the
[reference guide](claude-worker-in-a-container.md#put-the-project-on-container-local-storage-not-on-a--v-bind-mount-from-macos)
has the table.

There used to be a second rule here — *make sure the tree is owned by the worker
uid* — and a `chown -R 10001:10001 /srv/mission` to satisfy it. Both are gone.
You are uid 10001, so everything below is born owned by the identity that will
use it. There is no ownership step because there is no second identity.

```sh
mkdir -p /srv/mission && cd /srv/mission
git init -q
git config user.name  "container pilot"
git config user.email "pilot@cosmon.invalid"
git commit -q --allow-empty -m "empty base commit"

cs init                      # creates .cosmon/ — formulas, molecules, state
git add -A && git commit -qm "cs init"
```

Measured afterwards — no `chown` was run, and there was nothing for one to do:

```console
$ stat -c "%U:%G %n" /srv/mission /srv/mission/.git /srv/mission/.cosmon
cosmon-worker:cosmon-worker /srv/mission
cosmon-worker:cosmon-worker /srv/mission/.git
cosmon-worker:cosmon-worker /srv/mission/.cosmon
```

`cs init` on a brand-new galaxy will print one line you did not ask for:

```text
opt-in-share: stdin non-tty — question non posable ici, refus par défaut
enregistré (/root/.config/cosmon/consent.toml). Pour décider explicitement :
`cs opt-in-share --accept` ou `--decline`.
```

That is cosmon noticing it wanted to ask you something and that there is no
terminal to ask on, so it writes down *no* and carries on. Nothing is blocked
and nothing is shared. If you want to answer for real, run `cs opt-in-share`
with `--accept` or `--decline`. Until 2026-07-27 this question was posed into a
pipe nobody reads and stalled the dispatch for its full timeout; it now refuses
by default instead, which is why you get a line instead of a hang.

### The `safe.directory` step you no longer need

If you piloted as root, this is where you met git's *dubious ownership* refusal:
root operating a repository that belongs to uid 10001. The old fix was to grant
`safe.directory` to **both** users — and granting it only to root was the
classic half-fix, because it cleared the message you saw and left the worker,
the uid that actually has to commit, still refused.

On the non-root pilot there is nothing to exempt. One uid created the repository
and one uid operates it, so git's check passes on its own:

```console
$ git status --porcelain && echo "git is content"
git is content
```

That is not merely convenience. `safe.directory` suppresses a refusal; it grants
no access. Every time it appeared in this guide it was covering for an ownership
split we had created ourselves one step earlier.

## Step 6 — hand the workshop a job

```sh
cs nucleate task-work --var topic="add a one-line usage example to README.md"
```

```console
Nucleated molecule task-20260727-5909 from formula task-work
  Steps: 2
```

Before spending any quota, look at what the worker will actually be told.
`--dry-run` prints the brief and spawns nothing — measured, and it works with no
credential at all:

```console
$ cs tackle task-20260727-5909 --adapter claude --dry-run
# Autonomous work mode

You are a cosmon worker executing 🔧 molecule `task-20260727-5909`.
Formula: `task-work` — Step 1/2
…
## Mission

**add a one-line usage example to README.md**
```

If the mission does not read the way you meant it, fix it here. This is the
cheapest place to find out.

Then, for real:

```sh
cs tackle task-20260727-5909 --adapter claude
```

> **Not verified on this page.** The session that wrote this cannot hold a
> credential, so this exact line was run only as far as the credential
> refusal quoted in step 3. What is on the other side of it comes from the
> external tester's measurement on issue #20 (step 3), where two consecutive
> dispatches from a virgin config directory each produced a live worker and a
> committed artifact.

`cs tackle` prints its tmux socket and session name on the way out. Keep them —
that is your window into the worker.

`cs` here runs as uid 10001, the same uid the worker will run as, with no
`COSMON_WORKER_UID` anywhere. Nothing is demoted, because there is nothing to
demote from.

An earlier version of this page told you the opposite: pilot as root with
`COSMON_WORKER_UID=10001` and let cosmon demote the worker, on the grounds that
this was the shape three ownership bugs were found in. That was a real argument
about where to *look for* bugs, and it got mistaken for an argument about where
to *stand*. Four defects in two days later, the conclusion is the other one:
those bugs exist because a hand-over exists. Run without the hand-over and the
whole family is gone — see
[ADR-165](../adr/165-resources-are-created-under-the-identity-that-consumes-them.md).

The root → uid path is now **refused**, not merely discouraged. If you pilot as
root with `COSMON_WORKER_UID` set, `cs tackle` declines before it creates
anything and tells you to run as that uid instead:

```console
$ docker exec -e COSMON_WORKER_UID=10001 -w /srv/mission <container> \
    cs tackle task-20260727-5909 --adapter claude
Error: cs tackle: refusing to demote a worker from root to uid 10001: …
which would let this worker rewrite another molecule's branch or delete an
object another molecule's history depends on — run cs as uid 10001 itself
instead of as root … (molecule task-20260727-5909).
```

The reason is the one bug of the four that did not close by shortening or
lengthening the hand-over. Committing from a linked worktree needs write access
to the repository's *shared* object store and *shared* `refs/heads` — git offers
no way to hand over one branch or one object — so any grant large enough to let
a worker commit is also large enough to let it rewrite a sibling molecule's
branch and delete an object that molecule depends on. Both were reproduced, at
uid 10001 in a container and again at uid 501 on a laptop. Making that safe
means per-worker refs and objects with a controlled integration step, which is a
different worktree lifecycle and is not built. Until it is, the door is shut —
see [ADR-166](../adr/166-the-root-to-uid-demote-path-is-refused.md).

## Step 7 — watch the worker work

Remember the picture: the worker is inside the container, so watching it means
being inside the container too.

Open a **second door** into the same container:

```sh
docker exec -it -u 10001:10001 \
  -e HOME=/home/cosmon-worker \
  -e CLAUDE_CONFIG_DIR=/home/cosmon-worker/.claude-mission \
  -e LC_ALL=C.UTF-8 \
  -w /srv/mission cosmon-tutorial-02dc bash
```

Same door as the one you piloted through — same uid, same `HOME`, same config
dir. That is the point: there is only one shape of door on this page now.

Two views, and they answer different questions.

**"Is anything alive in here at all?"** — `cs peek` with **no argument**. This is
the fleet view, and it is the one to hand somebody who just wants to know
whether the thing is running:

```console
$ cs peek --once --no-tui
[18:05:35] BASELINE 0 worker(s), 1 molecule(s)
[18:05:35] + molecule task-20260727-5909  status: ○ pending  step: 0/2  worker: (none)
```

Drop `--once --no-tui` for the live TUI. Prefer this over `cs peek <id>`: the
bare form shows you the whole workshop, and "nothing is alive" is exactly the
answer you need to see quickly.

**"What is it doing right now?"** — attach to the worker's pane, using the socket
and session `cs tackle` printed:

```sh
tmux -L <socket> attach -t <session>
```

Copy the line cosmon printed rather than typing your own: when the environment
has no UTF-8 locale, cosmon puts the `LC_ALL=…` prefix into that printed line
for you. A hand-written attach in a POSIX shell is the underscore field from
trap 1.

Detach with `Ctrl-b d`. The worker keeps going; you were only looking.

## Step 8 — harvest

When the molecule reaches a terminal state, `cs done` merges the worker's branch
back and tears the worktree down:

```sh
cs done task-20260727-5909
```

> **Not verified on this page**, for the same reason as step 6: no live worker
> ever completed here. The git-plumbing ownership that this step depends on was
> repaired on 2026-07-27 (`482fe47`), after the tester found that a demoted
> worker could write its worktree but not the parent repository's
> `.git/worktrees/<molecule>` that a linked worktree commits through.

## Teardown

```sh
docker rm -f cosmon-tutorial-02dc
docker image rm cosmon-tutorial-02dc:local
```

That is the whole cleanup. The galaxy, the molecules, the worktrees, and — if
you chose the in-container login — the credential all lived inside the
container and go with it. Your laptop is as it was.

If you used a token instead, the token is still valid after this. Rotate it at
the source when you are done with it.

## What nobody has proven yet

Be careful what you conclude from this page.

What is proven, by the tester's own measurement, is **two short hands-off
dispatches**: virgin config directory, token-only auth, each producing a small
committed artifact, nothing touched in between.

What is **untested**, by anyone, so far:

- a mission running for hours rather than minutes;
- step transitions (`cs evolve`) accumulating over a long run;
- what quota draw looks like across a long mission;
- a worker dying mid-mission and being resumed;
- `cs done` closing the loop on a real long mission in a container.

If your plan depends on any of those, you are the first one there. Measure it,
and if it breaks, that is a finding worth filing rather than a mistake you made.

## Being honest about the door

This whole page drives a container through its shell. `docker exec` is a service
entrance.

cosmon has an invariant called `no-direct-shell` that exists precisely to avoid
this: work should arrive through typed commands with an audit envelope, not
through somebody typing into a prompt. Here we are typing into a prompt.

Three plain sentences about that, and then we move on:

- **It works, and it needs nothing installed.** No identity provider, no TLS, no
  reverse proxy. That is not a small thing when you are one person trying
  something out.
- **It is genuinely weaker than the front door on one axis and stronger on
  another.** A shell session leaves no audit record where an API request would
  leave one. But `docker exec` fails closed and loudly when you are not
  authorised, which is more than can be said for the lighter alternatives that
  keep getting proposed.
- **A proper API door for this shape does not exist yet.** Not "is discouraged" —
  does not exist. When it does, this page will change.

That is the trade. It is not sold as more than it is.

## Where to go next

- [Running a claude worker inside a container](claude-worker-in-a-container.md)
  — the reference: every gate, what it refuses, and why.
- [The issue-#20 container bench](container-worker-doors-bench.md) — the
  automated replay of the failures this page routes around.
- [Which engine do the container benches run on?](../benches/engine-fidelity-2026-07-27.md)
  — Colima vs Docker Desktop, measured.
