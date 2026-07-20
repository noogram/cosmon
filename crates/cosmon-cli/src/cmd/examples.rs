// SPDX-License-Identifier: AGPL-3.0-only

//! Per-subcommand `after_help` example blocks.
//!
//! These constants are attached via `#[command(after_help = ...)]` on the
//! `Command` enum variants in `main.rs` and also mirrored into `build.rs`
//! so `man cs` and `cs help <cmd>` stay symmetric.
//!
//! Style rule: every block starts with `EXAMPLES:` and contains at least
//! one runnable invocation. When a command has siblings that solve a
//! related problem (e.g. `kill` vs `quench`), we list them under `SEE ALSO:`
//! so operators find the right verb without leaving the help page.

pub const ENSEMBLE: &str = "EXAMPLES:
  cs ensemble                      # all molecules, all fleets
  cs ensemble --tag temp:hot       # actionable backlog snapshot
  cs ensemble --fleet research     # one fleet only
  cs ensemble --json               # NDJSON for scripting";

pub const DEMO: &str = "EXAMPLES:
  cs demo                              # interactive prompt → full cycle
  cs demo --prompt \"Implement X\"       # skip TTY, classify as task-work
  cs demo --formula deep-think --prompt \"Is X viable?\"
  cs demo --adapter llama-cpp --prompt \"Hello, world\"  # route through llama.cpp
  cs demo --no-teardown                # leave worktree intact for inspection

Runs nucleate → tackle → wait → done in one shot. All artefacts persist.

The --adapter flag is threaded to `cs tackle` so the demo cycle exercises
any registered Adapter (claude, aider, openai-chat, llama-cpp, …). Per
ADR-106 the legacy alias `llama` canonicalises to `llama-cpp` at the
CLI seam — both invocations route to the same in-process adapter.";

pub const NUCLEATE: &str = "EXAMPLES:
  cs nucleate task-work --var topic=\"refactor CLI help\"
  cs nucleate deep-think --kind deliberation --var question=\"...\"
  cs nucleate patrol --blocks root-mol         # add a DAG edge
  cs nucleate constellation --kind constellation \\
      --var pattern=\"three molecules re-invent the same missing primitive\" \\
      --var citations=\"delib-example-0001,task-example-0002,idea-example-0003\"
  cs nucleate --from molecules/               # hydrate from TOML declarations

KINDS (--kind):
  idea 💡, task 🔧, decision 📐, issue 🐛, signal ⚡,
  deliberation 🧠 (use with the deep-think formula),
  constellation 🌌 (fil-rouge artifact; see cs help guide).

LINK FLAGS:
  --blocks <id>       DAG progression edge (target cannot advance until this one completes)
  --blocked-by <id>   symmetric counterpart of --blocks
  --decayed-from <id> information edge: parent this molecule emerged from
  --refines <id>      citation edge (no progression semantics), auto-populated
                      from --var citations for --kind constellation

SEE ALSO: cs tackle (launch a worker on the molecule you just nucleated).
          docs/guides/constellation-pattern.md (when to use 🌌 vs 🧠).";

pub const OBSERVE: &str = "EXAMPLES:
  cs observe task-example-0001          # one-shot snapshot (never poll!)
  cs observe task-example-0001 --json   # for scripts

SEE ALSO: cs wait (block until terminal), cs peek (fractal TUI).";

pub const EVENTS: &str = "EXAMPLES:
  cs events --mol task-example-0001
  cs events --since 2026-04-13T00:00:00Z
  cs events --kind transition --json";

pub const EVOLVE: &str = "EXAMPLES:
  cs evolve <mol> --evidence \"step 1 done\" \\
      --formula .cosmon/formulas/task-work.formula.toml

Worker-callable. Advances the molecule one step per invocation.";

pub const COLLAPSE: &str = "EXAMPLES:
  cs collapse <mol> --reason \"superseded by <other>\"
  cs collapse <mol> --reason \"Claude usage limit reached\" \\
      --cause rate_limit --account default --kind max_rolling_5h

Terminal transition. Use instead of leaving stale pending molecules.

Pass --cause to attribute the failure with a structured tag (ADR-062):
  rate_limit  — quota refused; pair with --account ALIAS --kind CURRENCY
                (max_rolling_5h, max_weekly, api_key_org_monthly, …).
                Surfaces as ghost: quota-exhausted in cs peek.
  inference_stall  — worker alive but stopped emitting tokens.
  manual           — operator decision (default if --reason alone).
  process_death    — worker process died (OOM, signal).
  unknown          — could not be classified.";

pub const COMPLETE: &str = "EXAMPLES:
  cs complete <mol> --reason \"all steps done\"

Idempotent Active→Completed transition. Worker-callable. Does NOT merge
the branch or teardown the tmux session — use `cs done` for that.

SEE ALSO: cs done (merge + teardown), cs evolve (advance one step).";

pub const DECAY: &str = "EXAMPLES:
  cs decay <parent> --into task --var topic=\"subtask A\"
  cs decay <parent> --formula task-work --var topic=\"subtask B\"

1 → N. Children get a `DecayProduct` link back to parent. Remember to
`cs tag <child> --add temp:warm` so backlog curation works.";

pub const MERGE: &str = "EXAMPLES:
  cs merge <a> <b> <c> --into synthesis-<topic>
  cs merge <a> <b> --formula synthesis --var kind=decision

N → 1. Inverse of `decay`.";

pub const TRANSFORM: &str = "EXAMPLES:
  cs transform <mol> --to task          # idea → task
  cs transform <mol> --to decision

Preserves molecule ID; rewrites kind + formula bindings.";

pub const INIT: &str = "EXAMPLES:
  cs init                      # bootstrap .cosmon/ in the current directory
  cs init ./new-galaxy         # create ./new-galaxy/, then populate .cosmon/
  cs init --soft               # generate only CLAUDE.md (no .cosmon/)
  cs init --soft --template rust   # Rust-specific conventions
  cs init --soft --template data   # data/research conventions
  cs init --upgrade            # backfill missing canonical formulas

Creates `.cosmon/{config.toml, state/, formulas/, …}`. The target path
may not exist — `cs init` runs `mkdir -p` before populating. Running
twice on the same path is a strict no-op.

Does NOT run `git init` (that is git's job) and does NOT write
CLAUDE.md by default — pass `--soft` to generate an agent template.
Refuses to nest: if an ancestor already carries `.cosmon/`, exits
non-zero — no `--force`.

Symmetric undo: `rm -rf <path>/.cosmon/`.";

pub const KILL: &str = "EXAMPLES:
  cs kill worker-3            # DEPRECATED — see below

DEPRECATED (ADR-052 §D3): use `cs purge worker-3 --force` instead. This
alias will be removed after one release cycle.

SEE ALSO: cs purge (canonical), cs freeze (preserve state), cs done
(teardown after a molecule completes).";

pub const QUENCH: &str = "EXAMPLES:
  cs quench worker-3          # DEPRECATED — see below

DEPRECATED (ADR-052 §D3): use `cs freeze worker-3 --reason quench`
instead. Graceful shutdown with state preservation IS freeze; `--reason`
captures operator intent. Note: the canonical path lands in `Paused`
(resumable via `cs thaw`) rather than `Stopped`. This alias will be
removed after one release cycle.

SEE ALSO: cs freeze (canonical), cs purge (fleet teardown), cs teardown
(fleet-wide graceful shutdown).";

pub const FLEET: &str = "EXAMPLES:
  cs fleet list-templates     # installed templates
  cs fleet init research      # scaffold a .fleet.toml from a template
  cs fleet resolve            # flatten a composable fleet.toml (ADR-038)
  cs fleet resolve --json     # NDJSON for scripting

SEE ALSO: cs deploy (instantiate a fleet from the .fleet.toml).";

pub const FREEZE: &str = "EXAMPLES:
  cs freeze worker-3                           # suspend, keep state for later thaw
  cs freeze worker-3 --reason \"rotating OOM\"   # graceful shutdown with recorded intent

`--reason <str>` is the canonical replacement for the former `cs quench`
verb (ADR-052 §D3): graceful shutdown with state preservation IS freeze,
and the reason captures operator intent on the audit event.

SEE ALSO: cs thaw (resume a frozen worker), cs tackle (launch replacement
  after freezing an incumbent — priority inversion = freeze + tackle).";

pub const THAW: &str = "EXAMPLES:
  cs thaw worker-3            # resume a previously frozen worker

SEE ALSO: cs freeze (counterpart), cs resume (nudge idle workers).";

pub const PRIME: &str = "EXAMPLES:
  cs prime                    # load .cosmon/config.toml, report gates

Boot-time self-check. No network calls; verifies the local project is
well-formed.";

pub const RESUME: &str = "EXAMPLES:
  cs resume                   # nudge all idle workers
  cs resume worker-3          # nudge one

Convenience alias for `cs patrol --propel --molecule <id>` — maintains the
Propelled regime by re-delivering the propulsion signal. Does NOT change
state or advance molecules.

SEE ALSO: cs patrol --propel (canonical command), cs thaw (frozen workers).";

pub const TEARDOWN: &str = "EXAMPLES:
  cs teardown                 # gracefully stop every worker in the default fleet
  cs teardown --fleet research

SEE ALSO: cs kill (single worker, hard), cs quench (single worker, graceful).";

pub const PURGE: &str = "EXAMPLES:
  cs purge                    # sweep: remove Stopped / Error / Stale workers
  cs purge worker-3           # targeted: remove a single worker (graceful path)
  cs purge worker-3 --force   # targeted + SIGKILL tmux (supersedes `cs kill`)

ADR-052 §D3 collapses `cs kill` + `cs purge` into this one verb: both
are infrastructure teardown. Sweep mode (no argument) stays unchanged;
targeted mode with `--force` replaces `cs kill`.

SEE ALSO: cs freeze (graceful + state preservation), cs teardown
(fleet-wide graceful shutdown).";

pub const MIGRATE: &str = "EXAMPLES:
  cs migrate                           # legacy flat→fleet migration (pre-residence galaxies)
  cs migrate to solo                   # atomic data+git migration to solo residence
  cs migrate to team                   # move to team residence (seal → stage → verify → flip + git-side)
  cs migrate to team --dry-run         # seal manifest, print plan, touch no state
  cs migrate to solo --no-git          # skip the git-side half (data only — outside a git repo)
  cs migrate to solo --no-commit       # stage git-side changes, let operator commit
  cs migrate verify                    # re-walk state, compare against sealed manifest
  cs migrate rollback                  # inverse rename + restore git index / ignore files
  cs migrate rollback --dry-run        # preview the rename pair, touch nothing
  cs migrate genre github-surface --to solo --yes    # scoped: apply solo to one genre (ADR-057)
  cs migrate genre chronicle --to team --yes         # seed orphan branch cosmon/chronicle
  cs migrate genre github-surface --to solo --dry-run  # preview the plan, touch nothing

RESIDENCE VALUES:
  solo       local, single operator (default layout) — state goes into .git/info/exclude
  team       local, shared via git (cosmon-le-repo)  — state goes into .gitignore
  encrypted  local, encrypted at rest                — same gitignore rule as team
  remote     server-backed, network transport        — same gitignore rule as team

EXIT CODES (cs migrate verify, mirrors cs verify):
  0  manifest matches current state (A_pre ⊆ A_post, seal intact)
  1  divergence: offending entries listed on stderr
  2  no manifest on record (pre-migration galaxy or stale state)

The residence migration writes migration-manifest.pre.json at the galaxy
root before touching any state, then stages the new tree alongside as
state.next/, verifies it against the manifest, performs two atomic
rename(2) calls to flip, and finally runs the git-side half:
`git rm -r --cached` on the state directory, appends the state path to
the residence's ignore file (.git/info/exclude for solo, .gitignore for
team-class), and commits `chore(cosmon): migrate to <residence>
residence (git-side)` unless --no-commit is passed. state.prev/ is kept
as the rollback anchor; the pre-migration manifest also carries a
snapshot of the git HEAD and ignore files so rollback restores the git
side byte-for-byte. Orphan files (not tied to any molecule) are carried
in a distinct bucket and never silently discarded.";

pub const HEALTH: &str = "EXAMPLES:
  cs health                   # read-only anomaly catalog, current galaxy
  cs health --all             # every project below the configured root
  cs health --json            # NDJSON: one header line, one line per finding
  cs health --no-tmux         # state-only (skip the tmux liveness probe)

The Witness (ADR-137 Phase 1): a zero-mutation, control-plane-only scan
that surfaces the molecule-health anomaly catalog (A1 unsent-paste, A3
auth-dead, A4 idle-after-complete, A5 idle-running-zombie, A6 overloaded,
A7 ghost-merge, A8 completed-unharvested, A9 crash-zombie) the way
`cs peek` surfaces fleet state. Every signal is read from the state
machine — molecule status, liveness lease, transport probe — NEVER from a
pane glyph (the be1e use/mention guard). It heals nothing; the remedies it
prints are advisory. Exit code: 0 all-healthy, 1 findings present
(CI/monitor-friendly).";

pub const PULSE: &str = "EXAMPLES:
  cs pulse                    # runtime-vitality: RPM tachometer + six voyants
  cs pulse --window 10m       # widen observation window (default 5m)
  cs pulse --json             # cosmon.pulse/v1 NDJSON line (CI/scripting)

Pulse (ADR-138 Phase 1): a zero-mutation, stateless projection of fleet
liveness onto a single tachometer headline + six-voyant strip.

Headline: RPM = dΦ/dt = completions/min in the observation window W.
The event log is the pre-integrated derivative — no stored Φ, no new
state store (IFBDD). A word replaces the number when magnitude lies:
  SPINNING — tokens burn, Φ flat (P==0, B>b_min) → RED
  DRAINAGE OFF — no forward tick in τ → RED

Traffic light (first-match wins):
  RED   — subsystem dead (H_sched>τ) OR fuel exhausted OR spinning
  AMBER — stalled (P==0, L>0, ¬dead) OR starved molecules present
  GREEN — doing work (P>0) OR quiescent (L==0)

Voyant strip: scheduler / drainage / propel / heal / fuel / workers
A dead subsystem serializes 'off' (red-class) — never silently absent.

Exit code: always 0 (read-only, non-blocking — callers use --json state).";

pub const PATROL: &str = "EXAMPLES:
  cs patrol                   # run one health sweep
  cs patrol --propel          # nudge stale molecules
  cs patrol --respawn         # restart dead workers
  cs patrol --harvest         # close Completed-but-unmerged molecules
  cs patrol --silence-detect  # flag workers that stopped heartbeating
  cs patrol --livelock        # detect circular blocked-on waits
  cs patrol --event-age       # flag Running molecules with a quiet event log
  cs patrol --heal            # remediate safe anomaly classes, each §5-guarded
  cs patrol --heal --dry-run  # preview the Deacon's actions, mutate nothing

Designed to be run by an external scheduler (cron, launchd) in the
Propelled regime. `--harvest` is the belt-and-suspenders sweep that
complements the tmux pane-died hook installed at `cs tackle` time.
`--silence-detect`, `--livelock`, and `--event-age` are the
runtime-independent stall detectors: they read state from disk only, so
they fire even when `cosmon-runtime` is dead (the watchdog's liveness is
independent of the thing it watches). `--event-age` is the external-modal
backstop — it keys on the age of any event-log append, so it catches a
worker parked at a Claude Code AskUserQuestion modal that emits no
cosmon-visible state. Alerts are tiered by irreversibility: only an
irreversible-class block (signature/push/publish) fires `cs notify`.";

pub const PROJECT: &str = "EXAMPLES:
  cs project                  # materialize surfaces from the ledger
  cs project --check          # dry-run; exit 1 if surfaces are stale
  cs project --fetch          # pull current GitHub Issue state before comparing

Pure projection. Writes STATUS.md / ISSUES.md / GitHub issues per
.cosmon/surfaces.toml. Always safe to rerun. Reads as \"materialize views
from the ledger\" (ADR-052 §D3).";

pub const RECONCILE: &str = "EXAMPLES:
  cs reconcile                # DEPRECATED — see below

DEPRECATED (ADR-052 §D3): use `cs project` instead. The new verb reads
as \"materialize views from the ledger\"; `reconcile` read as \"patch
something that drifted\", which is the framing ADR-052 retires. This
alias will be removed after one release cycle.

SEE ALSO: cs project (canonical).";

pub const STATUS: &str = "EXAMPLES:
  cs status                   # pulse: active / pending / blocked / completed
  cs status --fleet research
  cs status --json            # includes `galaxies` block (by-kind + nascent)

SEE ALSO: cs peek (fractal TUI), cs ensemble (full snapshot),
          cs galaxies list (four-family taxonomy).";

pub const GALAXIES: &str = "IMAGE:
  The fleet of repositories is not a flat list — it is four families,
  classified by the direction the bits flow across the galaxy's boundary:

    infra        bits flow inward    the galaxy enables its sisters
    project      bits flow through   artefacts + illuminated principles
    social-hub   bits flow laterally human-to-human coordination
    editorial    bits flow outward   one-way publication to strangers
    nascent      not yet classified  awaiting W=28d observable tests

EXAMPLES:
  cs galaxies list              # grouped view, one section per family
  cs galaxies list --json       # flat array + `by_kind` totals

SEE ALSO: cs status --json (embeds the same `galaxies` block).";

pub const MUR: &str = "IMAGE:
  Le Mur du Matin est une fresque, pas un tableau de bord d'équipe.
  Quatre bandes horizontales empilées de haut en bas, pas une grille 2×2 :

    ═══ INFRA ══════════════════════════════════════════
        cosmon

    ─── PROJECT ────────────────────────────────────────
        showroom  earshot  cadence  mailroom …

    ─── SOCIAL-HUB ─────  ║  ─── EDITORIAL ─────────────
        showroom  incr.     ║      chancery

  La marge verticale vide entre social-hub et editorial est de
  l'information : elle signale une dérive si un hub dérive vers la
  publication ou inversement. Les positions sont fixes — l'opérateur
  mémorise les adresses comme on mémorise son quartier.

  Sept canaux visuels :
    • taille        log(activité) avec un plancher non-nul
    • couleur       santé, palette par famille (infra blue→red,
                    project green→red, hub rouge-saturé→gris,
                    editorial encre→crème)
    • position      fixe par galaxie
    • halo          épaisseur = North Star (succès, log-scaled)
    • mouvement     réservé à l'alerte de dérive
    • point rouge   ONE galaxie dont le North Star a bougé
                    > variance hebdomadaire hier
    • halo pointillé mesures plafonnées —
                     editorial reach, social-hub authenticity,
                     project principle-value — rendues avec une
                     ligne brisée plutôt qu'une ligne nette.

EXAMPLES:
  cs mur                   # fresque ANSI dans le terminal
  cs mur --json            # structure complète (tiles + delta leader)

SEE ALSO: cs galaxies list (la taxonomie sans la peinture),
          docs/handbook.md (chapitre 'Mur du Matin').";

pub const MOTION: &str = "IMAGE:
  La grande salle de contrôle : un tableau mural qui montre quels trains
  roulent, où, et à quelle vitesse. Cinq bandes verticales, toutes en
  temps quasi-réel (polling 3 s) :

    WORKERS             workers tmux live + molecule + heartbeat
    RUNNING MOLECULES   status=running, step N/M, last_evolve_at
    RECENT COMMITS      git log --since=<window> par galaxy
    RECENT WHISPERS     .md déposés dans .cosmon/whispers/inbox/ (30 min)
    RECENT SPARKS       spark-YYYYMMDD-xxxx créés pendant la fenêtre

  Agrégation cross-galaxy : tout ~/galaxies/*/ avec `.cosmon/`. Même
  mécanique que l'endpoint `GET /motion` de cs-api — la CLI lit le
  système de fichiers localement, pas de daemon, pas d'HTTP.

EXAMPLES:
  cs motion                        # snapshot ANSI (workers + molecules + commits + whispers + sparks)
  cs motion --watch                # refresh 3 s, clear-screen entre cycles
  cs motion --json                 # agent-first (utilise la même forme que /motion)
  cs motion --window 1h            # étend la fenêtre des sections 'recent'
  cs motion --galaxies cosmon,mailroom
  cs motion --include workers,molecules

SEE ALSO: cs-api GET /motion (même payload en HTTP),
          docs/guides/motion-view.md,
          cs peek (TUI fractal, plongée par worker).";

pub const SCHEDULER: &str = "IMAGE:
  cosmon-scheduler is the house's alarm clock. It looks at the wall
  clock every 60s, reads its tablet (`~/.config/cosmon/patrols.toml`),
  and asks: 'was anything supposed to ring now?'. If yes, it fires a
  short-lived command, the command finishes, it dies. Cron-like.

  (See the seven-clocks chronicle and the '2026-04-19 — Deux métiers,
  deux outils' chronicle for the full réveil/veilleur-de-nuit image.)

WHEN TO USE THE SCHEDULER:
  - periodic gesture, short burst (executor-pulse every 2h, mailroom-sync
    every 15min, chronicle-lint every Sunday morning)
  - fire-and-forget: the command finishes on its own
  - no persistent connection to maintain
  If the command must stay alive between fires, use `cs daemons` instead.

EXAMPLES — operator-facing (cs scheduler):
  cs scheduler status                   # pretty table of patrol last-fires
  cs scheduler status --json            # NDJSON, one object per patrol
  cs scheduler status --log-lines 20    # also tail last 20 log lines
  cs scheduler status --state-file /tmp/state.json
  cs scheduler validate                 # lint ~/.config/cosmon/patrols.toml
  cs scheduler validate --config cand.toml   # pre-flight a candidate file

CARDINAL PATROLS (copy-ready reference): docs/guides/patrols-cardinal.md
  cosmon-ward-mayor, reading-club-tick, leaks-watchdog,
  backlog-frontier-rot, digest-personnel — one [[patrol]] block each.

MINIMAL patrols.toml (~/.config/cosmon/patrols.toml):
  [scheduler]
  state_file            = \"~/.cosmon/scheduler.state.json\"
  log_file              = \"~/.cosmon/scheduler.log\"
  kill_switch           = \"~/.cosmon/stand-down.lock\"
  tick_interval_seconds = 60

  [[patrol]]
  name             = \"executor-pulse\"
  interval_seconds = 7200               # cadence: every 2h
  command          = [\"mailroom\", \"executor-pulse\"]
  enabled          = true

  [[patrol]]
  name        = \"chronicle-lint-weekly\"
  cron        = \"0 9 * * 0\"            # cadence: Sundays 09:00
  command     = [\"cs\", \"nucleate\", \"chronicle-lint\"]
  working_dir = \"~/galaxies/example-project\"
  enabled     = true

HOT-RELOAD:
  The scheduler re-reads `patrols.toml` on every tick (default 60s). Edit
  the file, save it, and the change takes effect within one tick — no
  signal, no reload command. Add a patrol = it fires on the next tick;
  disable one = it stops.

KILL-SWITCH:
  `touch ~/.cosmon/stand-down.lock` — the scheduler observes the lock at
  the next tick and quietly skips every patrol until the file is
  removed. No child is killed; already-firing patrols finish on their
  own. Same lock convention as cs daemons (one lockfile silences both
  worlds).

SEE ALSO:
  cs daemons (long-running processes), ADR-050 (unified patrol scheduler).";

pub const SECURITY: &str = "EXAMPLES:
  cs security status                  # show on-disk + recorded posture
  cs security activate                # prepared → active (warn → deny)
  cs security activate --rollback     # active → prepared (deny → warn, ~30s)
  cs security activate --dry-run      # preview the file edits
  cs security activate --no-commit    # write files, leave commit to operator
  cs security activate --no-push      # commit locally, push manually

Operator-only binary posture toggle for the supply-chain layer.
One gesture flips deny.toml + .github/workflows/deny.yml together —
no partial state.

Refuses to run inside a worker context (COSMON_MOL_DIR set): the
posture toggle is a kill-switch peer, manual, non-overridable.

Posture states:
  prepared — supply-chain gates wired, severity = \"warn\". WebAuthn
             câblé, required = false. Default shipping state.
  active   — severity = \"deny\" partout. WebAuthn required = true.
             Reached only by explicit operator gesture.

Records the chosen mode in ~/.config/cosmon/security.toml (cache, not
source of truth — the gate files on disk are authoritative).

SEE ALSO:
  ADR-076 (binary security posture),
  cs key (operator notary key).";

pub const SESSION: &str = "EXAMPLES:
  cs session start                             # open a carnet
  cs session start --galaxy example --root delib-example-0001
  cs session note \"Torvalds elected path a\"
  cs session note --tag insight \"carnet is the primitive\"
  cs session note \"!spark implémenter session-to-spark\"  # prefix auto-promotes
  cs session end                               # seal with BLAKE3 + auto-commit
  cs session end --no-seal                     # ephemeral scratch close

PROMOTE — turn session notes into spark molecules:
  cs session promote 10:46:55                  # promote one note by timestamp
  cs session promote 10:46:55 10:47:01         # promote several
  cs session promote --all-spark-prefix        # promote every !spark-prefixed note
  cs session promote --dry-run                 # show what would be promoted
  cs session promote --session session-2026-04-22T10-31-31Z 10:46:55

Notes beginning with `!spark ` are automatically promoted by the
session-to-spark LaunchAgent (when installed, fires every 5 min).
Explicit `cs session promote <ts>` works regardless of prefix and is
idempotent — sidecar markers under .cosmon/state/sessions/.promoted/
prevent duplicate sparks.

Exit codes:
  2    a session is already open (on `cs session start`)
  3    no open session (on `cs session note` / `cs session end`)

Sessions live under .cosmon/state/sessions/ as append-only markdown
files. The seal is a BLAKE3 hash of the body between the frontmatter
and footer — a trace, not a lock (architectural-invariants.md §8b).
Promotion never mutates a sealed session — markers are sidecar-only.";

pub const DAEMONS: &str = "IMAGE:
  cosmon-daemon-supervisor is the night watchman. It does not look at
  the clock. It looks at the dogs — processes that must always be
  alive. It reads its tablet (`~/.config/cosmon/daemons.toml`), keeps
  each dog alive; if one dies, it calls it back. Dogs never die
  voluntarily; if they die, it is an accident.

  (See the '2026-04-19 — Le gardien des chiens,
  et le gardien des portes' and '2026-04-19 — Deux métiers, deux outils'
  chronicles for the full gardien-de-chiens / veilleur-de-nuit image.)

WHEN TO USE THE SUPERVISOR:
  - long-running process, persistent connection (Telegram long-polling,
    IMAP IDLE, MCP stdio server, Emacs daemon)
  - must be restarted if it dies
  - throttle respawns to avoid crashloops
  Synthetic examples: notification-bot, archive-service, editor-daemon,
  metrics-dashboard.
  If the command should run once-and-exit on a cadence, use `cs
  scheduler` instead.

EXAMPLES — operator-facing (cs daemons):
  cs daemons list                       # declared daemons (from daemons.toml)
  cs daemons status                     # per-daemon status + last spawn age
  cs daemons status --json              # NDJSON, one object per daemon
  cs daemons reload                     # touch config → supervisor hot-reload
  cs daemons logs --lines 100           # tail the supervisor aggregate log

MINIMAL daemons.toml (~/.config/cosmon/daemons.toml):
  [supervisor]
  state_file  = \"~/.cosmon/daemon-supervisor.state.json\"
  log_file    = \"~/.cosmon/daemon-supervisor.log\"
  kill_switch = \"~/.cosmon/stand-down.lock\"

  [[daemon]]
  name             = \"notification-bot\"
  binary           = \"~/.local/bin/notification-bot\"
  args             = []
  throttle_seconds = 30
  env              = { RUST_LOG = \"info\" }
  log_stdout       = \"~/.local/state/notification-bot/stdout.log\"
  log_stderr       = \"~/.local/state/notification-bot/stderr.log\"
  enabled          = true

HOT-RELOAD:
  `cs daemons reload` touches `~/.config/cosmon/daemons.toml`. The
  supervisor's notify watcher picks up the modification, runs the diff,
  and restarts ONLY the daemons whose spec actually changed (debounce
  ~200ms). Adding a new `[[daemon]]` block makes that daemon appear;
  removing one makes it exit gracefully. No signal sent to the
  supervisor itself.

SUPERVISOR-OF-THE-SUPERVISOR:
  The supervisor itself is a long-running process — it needs its own
  watchman. That is `launchd` (macOS): scripts/install-daemon-supervisor.sh
  installs one LaunchAgent for the supervisor, and launchd keeps it
  alive. The supervisor keeps N dogs alive. One plist, N dogs.

  Install the LaunchAgent:
    scripts/install-daemon-supervisor.sh install
    scripts/install-daemon-supervisor.sh status
    scripts/install-daemon-supervisor.sh uninstall

KILL-SWITCH:
  `touch ~/.cosmon/stand-down.lock` — the supervisor SIGTERMs every
  child and parks them until the file disappears. Same convention as
  cs scheduler (one lockfile silences both).

SEE ALSO:
  cs scheduler (tick-based patrols), ADR-053 (cosmon-daemon-supervisor),
  ADR-016 §Autonomous (the regime this lives in).";

pub const TACKLE: &str = "EXAMPLES:
  cs tackle task-example-0001        # worktree + tmux + Claude (one node)
  cs tackle <mol> --dry-run           # print the bootstrap prompt
  cs tackle <mol> --no-worktree       # reuse current directory

`cs tackle` is ALWAYS leaf — it spawns one worker on the named node and
never walks the DAG. To walk a DAG of N≥1 nodes (1 = leaf, N = full
orchestration), use `cs run` instead. Human only. Workers never
self-tackle. Pairs with `cs done`.

The historical `--leaf` and `--force-runtime` flags are deprecated
no-ops since the verb-unification: the routing decision is now the
verb itself, not a flag on a polymorphic command.

SEE ALSO: cs run (DAG walk), cs done (teardown), cs wait (block on completion).";

pub const TAG: &str = "EXAMPLES:
  cs tag <mol> --add temp:hot
  cs tag <mol> --remove temp:warm --add temp:frozen

Temperature tags govern backlog curation — see CLAUDE.md § Molecule
Temperature Tags.";

pub const CLAIM: &str = "EXAMPLES:
  cs claim <mol>                 # reserve before cs tackle <mol>

Writes the durable `hold:pilot` tag. The resident runtime defers to a
claimed molecule unconditionally, so a pilot can reserve pending work before
tackling it. Idempotent: claiming an already-claimed molecule is safe.

SEE ALSO: cs release (return work to the runtime), cs tackle (start work).";

pub const RELEASE: &str = "EXAMPLES:
  cs release <mol>               # let cs run consider it again

Removes the durable `hold:pilot` tag. Idempotent: releasing an unclaimed
molecule is safe.

SEE ALSO: cs claim (reserve work), cs run (resident runtime).";

pub const NOTE: &str = "EXAMPLES:
  cs note <mol> \"observed flaky tmux reattach under load\"

Append-only audit trail. Shown in `cs observe` and `cs peek` notes tab.";

pub const WHISPER: &str = "EXAMPLES:
  cs whisper <mol> --message \"check the latest ADR before merging\"
  cs whisper <mol> --file hint.md
  echo \"nudge\" | cs whisper <mol> --stdin
  cs whisper <mol> --message \"…\" --dry-run      # validate, do not paste

Experimental v0. Perturbation port, not a control-plane event.
Refuses unless the target pane's foreground command is in
`[whisper] allowed_commands` (default: [\"claude\"]).";

pub const DONE: &str = "EXAMPLES:
  cs done task-example-0001                      # merge + teardown
  cs done <mol> --strategy ff-only                # linear history; attribution off
  cs done <mol> --force                           # skip completion check
  cs done <mol> --if-completed                    # silent no-op if not Completed

Not-the-worker. Legitimate callers: humans, external schedulers
(cron/launchd), and transport watchdogs (tmux pane-died hooks via
`cs done --if-completed`). Required to close the nucleate → tackle →
wait → done cycle: without it the branch never merges and the tmux
worktree persists.

`--if-completed` is the hook-friendly gate: exits success without
touching state when the molecule is not `Completed` or already merged;
behaves identically to plain `cs done` otherwise. Supersedes the
former `cs harvest` verb (ADR-052 §D3).

SEE ALSO: cs complete (state transition only), cs tackle (counterpart).";

pub const HARVEST: &str = "EXAMPLES:
  cs harvest --molecule task-example-0001      # DEPRECATED — see below

DEPRECATED (ADR-052 §D3): use `cs done --if-completed <mol>` instead.
The canonical path carries byte-identical semantics: silent no-op when
the molecule is not `Completed` or already merged; full teardown
otherwise. This alias will be removed after one release cycle.

SEE ALSO: cs done --if-completed (canonical), cs patrol --harvest
(belt-and-suspenders sweep).";

pub const STITCH: &str = "EXAMPLES:
  cs stitch <root-id>                    # merge mission's DAG into base under one lock
  cs stitch <root-id> --dry-run          # show planned order, do not touch git
  cs stitch <root-id> --cargo-check      # BLOCKING gate: red check rolls the merge back
  cs stitch <root-id> --push             # `git push origin <base>` after each merge

Walks the DAG closure
rooted at <root-id> via DagPolicy::compile_plan, sorts leaves → root
(Kahn toposort), acquires the trunk lock, then iterates:
  • `git merge --no-ff --no-edit <feat/<mol>>` into base (default `main`)
  • on textual conflict: `git merge --abort`, surface files
  • on untracked debris that duplicates branch content: auto-discard
    and retry; if it differs, report `untracked_overwrite` (NOT a conflict)
  • `--cargo-check` is a GATE, not a label: a red check rolls the merge
    back to its pre-merge SHA (`git reset --hard`), withholds the push,
    and the run exits non-zero (broken-trunk fix, 2026-06-11)
  • a failure poisons only its downstream lineage — independent DAG
    branches keep stitching (no global halt on the first conflict)
  • `--push` is a soft warning (no remote / network hiccup ≠ code failure)

`cs stitch` is the merge half of `cs done` — no worktree removal,
no branch deletion, no tmux teardown. Pair with `cs done` afterwards
when you also want the per-leaf cleanup. SEE ALSO: cs done, cs run.";

pub const STUCK: &str = "EXAMPLES:
  cs stuck <mol> --reason \"waiting on ADR-30 decision\"

Terminal-ish: freezes the molecule with a recorded blocker. Consider
`cs tag <mol> --add temp:frozen` for backlog hygiene.";

pub const AWAIT_OPERATOR: &str = "EXAMPLES:
  cs await-operator <mol> --question \"Sign and transmit the act, or revise?\"
  cs await-operator <mol> --question \"Push to the shared remote?\" --question \"Tag v1.2?\"

Worker-callable (ADR-123). The ONLY sanctioned way to block on an operator
decision at an IRREVERSIBLE boundary (signature transmitted, push to a shared
remote, publish, an authoritative value downstream consumers act on). NEVER
raise an off-cosmon modal (AskUserQuestion) — it is invisible to cosmon and the
DAG stalls silently.

Routes on the molecule's `op-block:<boundary>` capability (granted at nucleation
via `cs nucleate --may-block-on-operator <boundary>`):
  • capability present  → BLOCK: writes blocked_on.json, emits
    worker_blocked_on_operator, tags temp:awaiting-op, and yields. Molecule
    stays Running.
  • capability absent    → SURFACE-AND-CONTINUE: writes responses/needs-review.md
    and tells you to pick a sensible default and keep working (reversible).

SEE ALSO: cs nucleate --may-block-on-operator, cs patrol --event-age.";

pub const HEARTBEAT: &str = "EXAMPLES:
  cs heartbeat worker-3 --status thinking
  cs heartbeat worker-3 --status waiting --detail \"cargo test\"

Worker-callable. Emitted periodically so `cs patrol` can spot stalls.";

pub const NOTIFY: &str = "EXAMPLES:
  cs notify \"v1.2.1 tenant-demo deploy ok\"                    # default channels
  cs notify \"worker quartz silent 240s\" --level warn --molecule cs-20260426-a7e6
  cs notify \"hello\" --channel macos --channel file-drop      # override channels
  cs notify \"automata gen 4096\" --channel telegram          # Telegram DM
  cs notify \"dry test\" --dry-run                              # don't dispatch

Pushes one line to every channel in [notify].channels of .cosmon/config.toml
(macos | file-drop | element | telegram). Best-effort: a single channel failure is
logged but the others still fire. Closes the silent-24h gap by giving the
fleet a primitive to reach the operator's attention surface.";

pub const TOPOLOGY: &str = "EXAMPLES:
  cs topology map                      # PageRank-ranked module graph
  cs topology outline crates/cosmon-core/src/lib.rs
  cs topology symbols MoleculeId

Thin wrapper over the `topon` CLI. Structural view, not runtime state.";

pub const DEPS: &str = "EXAMPLES:
  cs deps <mol>                       # upstream + downstream blockers
  cs deps <mol> --upstream            # only predecessors
  cs deps <mol> --json                # for scripting

Reads the typed-link DAG (`Blocks` / `BlockedBy`).";

pub const PEEK: &str = "EXAMPLES:
  cs peek                         # TUI over the current .cosmon/
  cs peek --phase done,failed     # + the archive; project scope unchanged
  cs peek --all-galaxies          # same phases, every .cosmon/ + tmux socket
  cs peek --all                   # sugar for --all-galaxies --phase all
  cs peek --no-tui                # plaintext event stream
  cs peek --snapshot              # byte-deterministic 120-col canonical view
  cs peek --snapshot > /tmp/a     # capture from any device, then `diff` two
                                  #   captures and expect zero bytes (see
                                  #   docs/guides/peek-snapshot.md)

Keys in the TUI: j/k navigate, p tmux pane capture, b/l/e/s/r/n/g
briefing/log/events/synthesis/responses/notes/git tabs.";

pub const WAIT: &str = "EXAMPLES:
  cs wait <mol>                           # block until terminal
  cs wait <mol> --timeout 600             # 10-minute cap
  cs wait <mol> --status Completed        # custom target set
  cs wait <mol> &                         # background wait, notified on exit

This is kubectl-wait, not kubectl-watch. One molecule, bounded poll,
exits on target. Never poll `cs observe` in a shell loop.

SEE ALSO: cs observe (snapshot), cs peek (live fleet view).";

pub const RUN: &str = "EXAMPLES:
  tmux new -d -s runtime cs run <root> --poll-interval 5
  cs run <leaf-id>                   # also valid: 1-node DAG = single dispatch
  cs run <root> --force-runtime      # bypass the ADR-048 backlog-sanity guard

Resident runtime — walks a DAG of N≥1 nodes. Calls `cs tackle` for each
ready node and `cs done` automatically as predecessors complete. The
single-node case (1 = leaf) is the same code path as the N-node case;
`cs tackle <id>` is the no-walk equivalent if the operator wants exactly
one worker without any runtime ceremony.

NEVER run `cs run` in the foreground; always detach via tmux so the
pilot stays responsive.

SEE ALSO: cs tackle (single node, no runtime), docs/handbook.md#one-primitive.";

pub const SPORE: &str = "EXAMPLES:
  cs spore validate ./spore.toml --var subject=\"octopus cognition\"
  cs spore run ./bundle/ --var subject=\"...\" --var axes=a,b,c
  cs spore run ./spore.toml --allow-unchecked-seal     # sealed, no TLC
  cs spore export ./spore.toml --out dist/             # bundle hash + ASTRA
  cs spore validate ./spore.toml --json                # NDJSON expansion

VERBS:
  validate   parse + expand as a dry run; prints the ordered nucleate
             call list, germinates nothing.
  run        parse + expand + seal gate, then germinate the polymer into
             the live state store. --json emits one NDJSON line per molecule.
  export     content-addressed bundle hash + ASTRA descriptive layer (D6).

SEAL (ADR-140 D4): a sealed spore fails closed unless --allow-unchecked-seal
is passed; the status line never claims 'verified' when TLC did not run.

SEE ALSO: cs nucleate (one molecule), ADR-140, docs/design/spore-impl-dag-manifest.md.";

pub const SPORE_VALIDATE: &str = "EXAMPLES:
  cs spore validate ./spore.toml --var subject=\"octopus cognition\"
  cs spore validate ./bundle/ --var axes=a,b,c        # directory ref
  cs spore validate ./spore.toml --json               # NDJSON expansion

Dry run only: parse (N2) + expand (N3), print the ordered
'cs nucleate ... --blocked-by ...' call list, germinate nothing. The seal
is reported but never gated here; use it to inspect what 'cs spore run'
would create. Each --var is coerced into its declared ParamSchema type
before expansion; a list<string> param splits on commas.

SEE ALSO: cs spore run, cs spore export, ADR-140 D3.";

pub const SPORE_RUN: &str = "EXAMPLES:
  cs spore run ./spore.toml --var subject=\"...\" --var axes=a,b,c
  cs spore run ./bundle/ --fleet default               # directory ref
  cs spore run ./spore.toml --allow-unchecked-seal     # sealed, no TLC
  cs spore run ./spore.toml --json                     # one NDJSON line/molecule

Germinates the whole polymer: parse + expand + seal gate, then replays the
call list against the live state store via the canonical 'cs nucleate'
path. Every germinated molecule is tagged temp:warm and wired to its
blocked-by predecessors. The seal status note goes to stderr so --json
stdout stays clean NDJSON.

SEAL (ADR-140 D4): a spore with no [spore.seal] germinates freely. A sealed
spore fails closed unless --allow-unchecked-seal is passed, in which case
the status line reads 'seal: present, NOT verified' and never 'verified'.

SEE ALSO: cs spore validate (dry run), cs run (DAG of existing molecules).";

pub const SPORE_EXPORT: &str = "EXAMPLES:
  cs spore export ./spore.toml             # bundle hash to stdout
  cs spore export ./spore.toml --out dist/ # ASTRA into dist/
  cs spore export ./bundle/ --json         # machine-readable

Emits a content-addressed bundle id over the manifest and every recipe
and seal file it references (BLAKE3, sorted paths), plus an
ASTRA-compatible RO-Crate descriptive layer (ADR-140 D6) when
[spore.astra].emit is true. The seal verdict is attached honestly: marked
present/absent and never claimed verified. The bundle hash is stable: the
same bundle content always yields the same id (content-addressing is the
registry, ADR-039).

SEE ALSO: cs spore run, ADR-140 D6, ADR-039.";

pub const HELP: &str = "EXAMPLES:
  cs help                         # grouped command reference
  cs help tackle                  # detailed help for one command
  cs help guide                   # embedded operator handbook
  cs help charter                 # visual charter swatch";

pub const REPLAY: &str = "EXAMPLES:
  cs replay                       # spin up the D3 timeline on localhost
  cs replay --since 2026-04-01    # filter by start date

Interactive post-mortem view of events.jsonl.";

pub const VERIFY: &str = "EXAMPLES:
  cs verify <mol>                 # walk the event hash chain
  cs verify <mol> --strict        # also replay gates

Proof-of-work chain integrity check.";

pub const VERIFY_GRAPH: &str = "EXAMPLES:
  cs verify-graph --relation blocks         # check the Blocks subgraph for cycles
  cs verify-graph --relation refines        # check Refines (cycles permitted, reported as WARN)
  cs verify-graph --all                     # every registered relation
  cs verify-graph --all --json              # NDJSON, one row per relation

Tarjan SCC check on the subgraph induced by a typed MoleculeLink
relation. Substrate primitive for the organization-twin programme.
Read-only — does not mutate state.

Exit code:
  0  every DAG-required relation is acyclic
  1  at least one DAG-required relation contained a cycle
  Cycles in non-DAG-required relations (e.g. `refines`) are
  reported but do not flip the exit code.";

pub const SPEC_AUDIT: &str = "EXAMPLES:
  cs spec-audit                          # default: cosmon-run × events.jsonl
  cs spec-audit --fleet default          # explicit fleet (advisory today)
  cs spec-audit --events path/to/events.jsonl
  cs spec-audit --json                   # NDJSON drift report
  cs spec-audit --no-git-probe           # skip the c1cb merge-topology probe

Multi-spec:
  cs spec-audit --spec mycelial-gate     --events .cosmon/state/attestor-events.jsonl
  cs spec-audit --spec attestor-graph    --events .cosmon/state/attestor-events.jsonl
  cs spec-audit --spec witness-freshness --events .cosmon/state/attestor-events.jsonl
  cs spec-audit --spec noogram/specs/MycelialGate.tla   # path also accepted

Ledger audit: replays events through the TLA+ spec and flags drifts. For
--spec cosmon-run (default), drifts include c1cb bypass_merge and
disabled-action-fired. For the noogram specs, drifts are emitted as
spec_invariant_violation with stable (spec, invariant) tags
— see crates/cosmon-core/src/attestor_audit.rs for the full taxonomy
and docs/specs/attestor-events.schema.json for the AttestorEventV1
NDJSON schema. One-shot, not a daemon.
Exit 0 if clean, 1 if any drift is found.";

pub const RELEASE_AUDIT: &str = "EXAMPLES:
   cs release-audit --dry-run             # simulate the release chain on the live tree
   cs release-audit --dry-run --json      # machine report for CI / jq
   cs release-audit --repo /path/to/clone # audit an explicit repo root

Dry-run drift detector for the public cosmon distribution — the release-side
analogue of `cs reconcile --check`.

PRIMARY MEMBRANE — deny-by-default allowlist (ADR-127). Ship nothing except
positively-cleared paths: every tracked, non-purged path must carry a per-path
permit in .cosmon/release-allowlist.toml, or it is a `path-not-permitted`
regression. A new confidential file is caught BY CONSTRUCTION (new = unpermitted
= refused), instead of slipping past a frozen denylist. Content-bound permits
(with a blake3 `seal`) go `permit-stale` when the file changes — cleanliness-now,
not freshness-at-t0. The membrane is ARMED by the presence of the allowlist
file; absent it, the audit runs in legacy denylist mode and says so LOUDLY
(a warning, never a silent pass). Bless paths with scripts/release/
bless-allowlist.sh (a separate tool — the audit stays read-only).

CONTENT BACKSTOP — the legacy detectors still run on permitted files:
  - a private-sibling path dependency reappeared (the claudion vendoring case);
  - a client name reintroduced in a tracked path the rename chain misses;
  - a structural string the chain does not scrub (operator homeserver, etc.);
  - a live instance oidc-identity.toml re-tracked under a non-purged path.
The confidential detector literals live in the PRIVATE, purged-from-release
.cosmon/release-rules.toml (Bucket-3) — not in the shipped source, so the
detector is no longer its own leak. Absent that file the backstop is inert and
the audit warns.

Exit 0 if the distribution is clean, 1 if it would regress. One-shot, not a
daemon; reports, does not remediate. See ADR-127.

The audited repo may exempt structural strings that are intentionally public in
it (e.g. a maintainer-contact domain) via .cosmon/release-audit.toml, each with
a mandatory justification — the same exemption list its own forbid-strings CI
gate should read, so both referees agree.";

pub const TEST: &str = "EXAMPLES:
  cs test --binding-report        # constitution ↔ scenario binding audit

Spec-suite entry point only. This is NOT a wrapper around `cargo test`
— use `cargo test --workspace` for unit/integration tests.";

pub const ARCHIVE: &str = "EXAMPLES:
  cs archive list                         # every archived molecule, all months
  cs archive list --year 2026             # scoped to one year
  cs archive list --year 2026 --month 04  # scoped to one month
  cs archive list --json                  # NDJSON for scripting
  cs archive show <mol>                   # manifest + artifact inventory
  cs archive verify <mol>                 # recompute hashes (exit 1 if tampered)
  cs archive prune --dry-run              # what retention policy would delete
  cs archive prune                        # execute the retention policy

Operator view onto `.cosmon/state/archive/`. Terminal transitions
(`cs done` / `cs collapse` / `cs freeze` / `cs stuck`) populate the
archive when `[archive] enabled = true` in the project config. The
archive outlives worktree teardown and branch deletion — a fresh
clone sees every merged molecule's canonical snapshot.

Retention is controlled by `[archive.retention]` in config.toml:
  keep_all      (default true)  — safety switch; must be false to delete
  max_age_days  (default 0)     — 0 disables the age rule
  max_total_mb  (default 0)     — 0 disables the size rule
  keep_kinds    (default decision, deliberation)

Hash-chain integrity is enforced: a molecule referenced as parent
(DecayedFrom / BlockedBy / MergedFrom) by a kept entry is never
deleted.";

pub const OPT_IN_SHARE: &str = "EXAMPLES:
  cs opt-in-share             # first-run prompt (once per user)
  cs opt-in-share --status    # show current consent state
  cs opt-in-share --decline   # non-interactive: record decline
  cs opt-in-share --accept    # non-interactive: record acceptance
  cs opt-in-share --json      # NDJSON output for scripting

Deny-by-default. The first time `cs tackle` runs on a TTY, this prompt
fires automatically (once) and the answer is persisted to
~/.config/cosmon/consent.toml. No trace in your project's git log.

The French prompt names the encryption (age), the sole recipient
(the Noogram maintainer), and the no-trace-in-commits guarantee, then asks [o/N].
Anything but an explicit yes is recorded as a decline. When stdin is
not a TTY (CI, scripts, worker shells), the hook skips the prompt and
records a decline without asking.

SEE ALSO: cs tackle (the first-run hook site).";

pub const INSPECT: &str = "EXAMPLES:
  cs inspect docs/lore/2026-04-20-x.md      # classify one path
  cs inspect STATUS.md --verbose            # show the matched glob
  cs inspect src/main.rs --json             # NDJSON for scripting

Reads `.cosmon/artifact-map.toml` (ADR-057), matches the path against
declared globs (longest-fixed wins), prints the genre, audience,
derived residence, and computed `rot` (days since last commit).

When the TOML is absent, every path falls through to the `code`
catch-all (backwards-compatible default).

SEE ALSO: cs artifacts audit (walk git ls-files), ADR-057.";

pub const ARTIFACTS: &str = "EXAMPLES:
  cs artifacts audit                # walk git ls-files, count per genre
  cs artifacts audit --json         # NDJSON: one line per genre + summary

Audit exits 0 when the four ADR-057 invariants hold (every tracked
path classifies); exits 1 on any I1 totality violation.

SEE ALSO: cs inspect (classify one path), docs/guides/artifact-map.md.";

pub const CLUSTER: &str = "EXAMPLES:
  cs cluster show                                    # pretty-print the resolved config
  cs cluster show --json                             # machine-readable form
  cs cluster edit                                    # $EDITOR on the file (seeds a template if absent)
  cs cluster bootstrap http://192.0.2.10:4222      # fetch + install from a peer

The cluster.toml file lives at ~/.config/cosmon/cluster.toml and describes
the **topology** of your cosmon cluster (hosts, Tailscale IPs, surfaces
like cs-api and matrix-echo-tick, app bundle ids).

It is **references-not-secrets** by design (ADR-066): paths to credential
files appear here, credentials themselves never do.

New device onboarding: run `cs cluster bootstrap <primary-url>` on the new
device — it fetches /cluster from the primary's cs-api and caches the
topology locally. No code rebuild, no hardcode.

SEE ALSO:
  docs/guides/surfaces-cluster-config.md
  ADR-066 (docs/adr/066-surfaces-cluster-config.md)";

pub const SPARK: &str = "EXAMPLES:
  cs spark \"réunion demain : revoir le pitch\"
  cs spark \"debug the flaky test\" --kind task
  cs spark \"CI broken on macOS\" --kind issue --tag temp:warm
  cs spark \"constellation idea\" --nucleon operator-demo@example.com

A spark is the pre-task — a one-line operator intent dropped into the
Inbox (HOT bucket by default) with the sparker's identity attached. The
demo criterion: operator-demo on her iPhone via
Blink Shell SSH types a single spark and the operator sees it on the
next 'cs peek' refresh. No Claude Code in the chain.

DEFAULTS:
  --kind idea                # 💡 one-line Inbox shape
  --tag  temp:hot            # surfaces in 'cs inbox' HOT bucket
  nucleon_id: git user.email, else $USER@$(hostname)

SACRIFICES: offline fails, auth ≡ SSH access, no push (next
refresh), raw Blink terminal UX, v1 is one-way (no pilot reply from
Inbox).

SEE ALSO: cs inbox (where sparks land), cs tackle (promote to work),
          cs transform (re-kind), cs collapse (reject with reason).";

pub const KEY: &str = "EXAMPLES:
  cs key generate                              # ~/.config/cosmon/operator.key (0600)
  cs key generate --output /tmp/dev.key        # alternate destination
  cs key generate --force                      # overwrite an existing key
  cs key show                                  # print pubkey hex of the default key

Generates a fresh Ed25519 secret (32 bytes of OS randomness, 64-char
lowercase hex) at the path `cs notarize --key` already expects. Silent
rotation is forbidden: without `--force` the command refuses to
clobber an existing key file. For retirement / successor publication,
see ADR-060 (`cs rotate-key`, deferred post-S4).

SEE ALSO:
  cs notarize (sign a molecule under the operator key),
  docs/guides/notary-operator-guide.md, ADR-056, ADR-060.";

pub const INBOX: &str = "EXAMPLES:
  cs inbox                         # vertical pile of atomic actions
  cs inbox --json                  # NDJSON: one row per actionable molecule

The pile has four buckets, top to bottom:
  ✓ completed (awaiting `cs done`)    🔥 temp:hot pending
  ❓ frozen (question from a worker)   ⚡ signal molecules

Keys: j/k move · Enter open briefing · d done · t tackle · w whisper
      · c collapse · r reload · q quit. One panel, one stack — no graph
viewer, no chat, no split, no editor, no dashboard, no search bar — the
deliberate non-features.

Success metric: 5 days out of 7 without opening Claude Code
to pilot cosmon. See docs/guides/inbox-trial.md.

SEE ALSO: cs ensemble (full backlog), cs peek (fractal TUI portal),
cs session (operator carnet that inbox reads as its sticky top line).";

pub const DROP: &str = "EXAMPLES:
  cs drop                     # universal Inbox gesture — captures whatever is in scope

Equivalent of `cs spark` reachable from hotkey / zsh widget / menubar.

SEE ALSO: cs spark (canonical), cs nucleate spark (formula form).";

pub const LISTEN: &str = "EXAMPLES:
  cs listen                   # voice-driven spark capture (whisper.cpp)
  cs listen --once            # single utterance, then exit

Voice ingress to the fleet. Wraps whisper.cpp transcription and
produces a `spark` molecule per utterance.

SEE ALSO: cs spark, cs drop.";

pub const TAIL: &str = "EXAMPLES:
  cs tail                     # live tail of the current galaxy events.jsonl
  cs tail --all-galaxies      # multiplex all known galaxies

Live `notify`-driven reader over `events.jsonl`. Default scope is the
current fleet; --all-galaxies multiplexes across the registered
cosmon roots.

SEE ALSO: cs events (one-shot dump), cs ensemble (snapshot view).";

pub const DIVERGE: &str = "EXAMPLES:
  cs diverge <mol_id>         # structural agreement check between two sessions

A structural-agreement primitive: two independent sessions on
the same molecule are compared for divergence on the artifacts they
produced. Used by livelock detection and consensus checks.

SEE ALSO: cs observe, cs deps.";

pub const WITNESS: &str = "EXAMPLES:
  cs witness attest <mol>                       # default prior path: <mol_dir>/prior.md
  cs witness attest <mol> --prior-path prior.md # explicit prior file
  cs witness attest <mol> --witness-id ci-bot   # deterministic identity (LaunchAgent/CI)
  cs witness attest <mol> --json                # NDJSON for scripting

Layer-2 witness-quorum seal for stress-test molecules (ADR-085 §3).
A separate cosmon agent reads the prior file's bytes, computes its
BLAKE3 hash, and emits a SealAttested event distinct from the
molecule's tackler session. Refuses if the molecule's class is not
stress-test, or if the witness identity matches the tackler's
session_name (cheap structural-independence check).

SEE ALSO: cs notarize (operator Ed25519 attestation, ADR-056).";

pub const PANEL: &str = "EXAMPLES:
  git diff main | cs panel convene                   # seat the panel from the staged diff
  cs panel convene --diff change.patch               # seat from a diff file
  cs panel convene --diff change.patch --json        # NDJSON for scripting
  cs panel decide --diff change.patch \\
    --vote wheeler=approve --vote torvalds=refuse:breaks I1 \\
    --vote feynman=approve --vote shannon=approve \\
    --vote jobs=approve --out panel.role-log.json    # tally + inscribe role-log

Convene a hash-pinned supermajority panel to gate a *constitutional*
amendment — anything operator-uncapturable, a forbid_operator_* lint, or a
new operator_* field. The cost of amendment rises from O(1 PR) to
O(panel convocation): what was legislative becomes constitutional.

A panel is a fixed constitutional CORE (default wheeler,torvalds,feynman,
shannon) plus rotating SEAT(S) drawn from a POOL by hashing the diff. The
rotating seat is a pure function of the diff, so the convener cannot pick a
friendly judge after seeing the test (audience-after-the-test, delib
20260503-5a74). `decide` refuses ballots from non-panelists and refuses to
rule until every seat has voted. Verdict needs a 4/5 supermajority (--rule).

EXIT CODES (decide): 0 approve, 2 refuse, 1 error.

SEE ALSO: cs notarize (operator Ed25519 attestation), cs witness (quorum seal).";

pub const PRESENCE: &str = "EXAMPLES:
  cs presence ping            # heartbeat for current session
  cs presence ls              # list live sessions
  cs presence gc              # garbage-collect dead sessions
  cs presence poll            # one-shot poll, exit

Live-session registry — who is currently working in the fleet.
Sessions ping their liveness; `ls` enumerates; `gc` reaps the dead.

SEE ALSO: cs ensemble, cs peek (presence row in TUI).";
