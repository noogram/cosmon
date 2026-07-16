// SPDX-License-Identifier: AGPL-3.0-only

//! Long-form help text attached to the root `cs` command.
//!
//! Kept in a separate module so the rich multi-paragraph narrative
//! (pilot workflow, monitoring toolkit, scheduler/supervisor split,
//! archive) lives next to the `Cli` derive in `main.rs` without
//! drowning it. The constants are referenced from
//! `#[command(long_about = ..., after_long_help = ...)]` on `Cli`, so
//! `cs --help`, `cs help`, and the generated man page all read from
//! one source.

/// Long about attached to the root `cs` command.
///
/// Rendered verbatim by clap for `cs --help` (long form) and by
/// [`clap_mangen::Man`] for the `DESCRIPTION` section of the man page.
pub const LONG_ABOUT: &str = "Cosmon keeps a fleet of AI agents on track. Run several on one \
             codebase and a session will crash, or fill its context window and forget what it \
             was doing, and you lose track of which agent was on what — cosmon gives each a \
             durable identity and writes every step to disk, so a dead session resumes where it \
             stopped and you can always see who is doing what. More generally it runs missions \
             you delegate to AI: it breaks a hard problem into typed, ordered steps, executes \
             them, and records every step so the finished work carries an auditable trace — the \
             reasoning matters as much as the result. Nothing in it is code-specific; it aims \
             just as well at research-grade missions, where a technical lock demands long, \
             decomposed, checkable work. It is model-agnostic, \
             sitting above any model, so your context and data stay with you and a mission can \
             run entirely on local models. Cosmon is the kernel of noogram — the open-source \
             substrate for composing, piloting and auditing missions entrusted to AI systems.\n\n\
             PILOT WORKFLOW — the full cycle is nucleate → tackle → wait → done:\n  \
             cs nucleate <formula> ...       create molecule\n  \
             cs tackle <id>                  spawn ONE worker on this node — always\n                                  \
               leaf, never walks the DAG. The verb is\n                                  \
               picked by the operator: cs tackle = single\n                                  \
               node; cs run = N≥1 nodes (resident runtime).\n  \
             cs wait <id> &                  background wait, get notified on completion\n  \
             cs done <id>                    merge branch to main + teardown (required!)\n\n\
             Pilots NEVER poll 'cs observe' by hand — always use 'cs wait'. \
             Pilots NEVER skip 'cs done' — without it the branch never merges. \
             Pilots NEVER run 'cs run' in the foreground — wrap it in a detached \
             tmux session: tmux new -d -s runtime cs run <root> --poll-interval 5 \
             (cs run calls cs done automatically on completion). The historical \
             '--leaf' and '--force-runtime' flags on 'cs tackle' are deprecated \
             no-ops since the verb-unification: pick the \
             right verb instead.\n\n\
             ANTI-PREEMPTION LEASE. A human 'cs tackle' and a \
             running 'cs run' are two writers racing on the same Pending → Active \
             flip, so every tackle records WHO dispatched it: 'cs tackle --by human' \
             (the default) or 'cs tackle --by runtime:<pid>' (what 'cs run' passes). \
             The walker re-reads each candidate fresh from disk right before dispatch \
             and SKIPS anything no longer Pending, carrying a sticky 'human' claim, \
             or tagged 'hold:pilot'. Use 'cs claim <id>' before reaching for a pending \
             molecule: it writes the durable pilot hold before 'cs tackle', and \
             'cs release <id>' removes it. The runtime always defers to that hold. \
             'manual always wins', the runtime never raffles a molecule you reached \
             for. Binary owner field, no clock to calibrate. Operators do not normally \
             type '--by'; 'human' is the default.\n\n\
             SPORE (ADR-140) germinates a whole polymer from a shareable \
             'spore.toml' template, the way 'cs nucleate' germinates one \
             molecule. A spore is a parameterizable mission plan: a fleet, \
             per-node formulas, a ParamSchema, a DAG of typed edges, and an \
             optional '.tla' seal. Three verbs, one role each:\n  \
             cs spore validate <ref>         parse + expand as a dry run; prints\n                                  \
               the ordered nucleate call list,\n                                  \
               germinates nothing.\n  \
             cs spore run <ref>              parse + expand + seal gate, then\n                                  \
               germinate the polymer into the live\n                                  \
               state store (every node tagged\n                                  \
               temp:warm, wired to its blocked-by).\n  \
             cs spore export <ref>           content-addressed bundle hash +\n                                  \
               ASTRA descriptive layer (D6).\n\n\
             '<ref>' is a 'spore.toml' file or a directory containing one. \
             '--var k=v' (repeatable) binds a parameter, coerced into its \
             declared ParamSchema type before expansion. '--json' on validate \
             and run emits NDJSON (agent-first invariant). A spore is a \
             declarative front end over the existing 'cs nucleate' verb, not a \
             new scheduler and not a new molecule type.\n\n\
             SEAL GATE (ADR-140 D4), stated honestly: a spore with no \
             [spore.seal] germinates freely ('seal: none'). A sealed spore \
             cannot be proven on a machine without the TLC verifier wired in, \
             so 'cs spore run' fails closed by default and refuses unless the \
             operator opts into the risk with '--allow-unchecked-seal', in \
             which case the status line reads 'seal: present, NOT verified'. \
             It never claims 'verified' when TLC did not run. See \
             docs/cs-spore.md and docs/design/spore-impl-dag-manifest.md.\n\n\
             MONITORING — the operator's toolkit (use these, not tmux/tail/cat):\n  \
             cs peek                         watchdog TUI — every unfinished molecule\n                                  \
               (running, pending, frozen, starved; the\n                                   \
                archive is hidden by default; press A\n                                   \
                to cycle or pass --phase)\n  \
             cs peek --phase done,failed     + the archive (completed + collapsed)\n  \
             cs peek --all-galaxies          same phases, every project\n  \
             cs peek --all                   sugar for --all-galaxies --phase all:\n                                  \
               everything, every project, archive\n                                  \
               included (multi-galaxy)\n  \
             cs peek --snapshot              byte-deterministic 120-col canonical view\n                                  \
               (same bytes on any device; cf.\n                                   \
               docs/guides/peek-snapshot.md)\n  \
             cs peek --json                  machine view: one JSON document,\n                                  \
               printed once, sorted by molecule id\n  \
             cs ensemble --tag temp:hot      actionable backlog snapshot\n  \
             cs wait <id> &                  block on a worker without hanging the pilot\n\n\
             Observability is a fractal portal, not a dashboard: one tool (cs peek), \
             recursive, from fleet overview down to per-molecule artifact. Reach for \
             'cs peek' before 'tmux attach', 'tail -f', or 'cat briefing.md'.\n\n\
             cs peek navigates three continuous scales via zoom keys: '+' steps in \
             (ville → immeuble → peau), '-' steps out, '=' resets to ville. Ville \
             is the fleet table; immeuble is one molecule pleine-page with adjacent \
             neighbours wired by straight monospace DAG cables; peau is the raw \
             artifact (briefing / synthesis / log / …) at full resolution. See \
             docs/guides/peek-zoom.md.\n\n\
             cs peek --json — the machine projection. One JSON document, printed \
             once: {\"filter\", \"molecules\": [{\"id\", \"project\", \"status\", \
             \"heartbeat\", \"last_activity\", \"updated_at\"}]}, sorted by id so two \
             captures of an unchanged fleet diff to nothing. 'status' is the raw core \
             status — the same word 'cs observe --json' reports for the same molecule, \
             never re-lettered, and an unrecognised future status serialises as its own \
             snake_case name rather than being laundered into 'pending'. 'heartbeat' is \
             the liveness tier (active|idle|quiet|stalled|orphaned); 'last_activity' is \
             the timestamp it was classified from, but it folds in tmux's attach-bumped \
             session clock, so merely attaching moves it — 'updated_at' moves only on a \
             state write and is what a stall or orphan patrol should read. 'filter' names \
             the published slice, so absence is distinguishable from filtered-by-design. \
             'project' is a display label, not a join key. '--phase'/'--all' select the \
             same molecules as on screen; '--json' wins over '--snapshot'. No bucket \
             field: that taxonomy is under active redesign, and publishing it would \
             freeze a machine contract to the one object with a demonstrated re-cut \
             cadence. Token counts live in 'cs ensemble --json'.\n\n\
             cs peek also drives the fleet from inside the TUI. Cockpit action \
             keystrokes open a thin modal that fires one 'cs <verb>' and waits: \
             'n' nucleate (formula + topic), 't' tackle the selected row, 'm' \
             merge-and-done (cs done) with y/N confirmation, 'w' whisper a body \
             to the selected worker, '.' append a session note. Esc cancels any \
             modal. Detail-pane letters 'n' and 't' moved to 'N' (notes) and 'T' \
             (tree); the mouse-capture toggle moved from 'm' to 'M'.\n\n\
             cs peek — presence header strip. When '.cosmon/state/presence/*.json' \
             files exist, a one-line strip under the title lists every live Claude \
             session (galaxy \"headline\"). Stale entries (heartbeat > 3 min) render \
             greyed; files without a heartbeat are dropped. The strip is a read-only \
             wheat-paste over the presence registry owned by C-PRESENCE-CORE ('cs \
             presence ping|ls|gc').\n\n\
             cs peek — ensemble tab ('E' key). Hot-swaps the molecule table for a \
             fleet-wide events view, newest-first. Columns: ts | mol_id | kind | \
             summary. 'j/k' scrolls; Enter closes the tab and zooms on the selected \
             molecule. Useful above N≈50 live molecules when the per-row detail \
             panes drown the signal.\n\n\
             cs peek — non-interactive filter flags: '--filter <str>' \
             pre-populates the TUI '/' field; '--tag <t>' restricts to molecules \
             carrying that tag; '--since <ts|dur>' hides rows older than an \
             RFC-3339 timestamp or a duration suffix ('30m', '2h', '3d'); \
             '--since-event N' keeps only the last N events per molecule in the \
             ensemble view. Together they keep cs peek readable at large N.\n\n\
             cs peek — phases. Every molecule sits in exactly one phase, and the \
             phase is a total function of its status: 'live' (running), \
             'waiting' (pending + queued), 'blocked' (starved — an external \
             authority refused service, ADR-062), 'parked' (frozen), 'failed' \
             (collapsed) and 'done' (completed). The TUI table and the \
             '--no-tui' baseline both default to the unfinished ones — every \
             phase that is not 'failed' or 'done'. The archive is what drowns \
             the daily signal; the frozen, starved and pending rows are the \
             signal, and no other instrument reports them. One flag per axis \
             widens the slice: '--phase' selects the phases ('--phase \
             done,failed' adds the archive), '--all-galaxies' selects the \
             projects, and neither touches the other's axis. '--all' is sugar \
             for '--all-galaxies --phase all' and is the only flag that speaks \
             to both. The interactive 'A' key \
             cycles the phase filter (unfinished → all → unfinished) with a \
             status-line label; the lowercase 'a' key still toggles the \
             all-projects scope independently. Live transition events (status change, step \
             advance, worker reassignment) are always shown in '--no-tui' \
             mode regardless of the filter — they are the signal the \
             operator subscribed to. See docs/guides/peek-temporalities.md.\n\n\
             REGISTERING A SERVICE — two métiers, two tools:\n  \
             cosmon-scheduler (patrols.toml)   the house's alarm clock. Periodic,\n                                    \
               short-lived gestures — cron/interval fires.\n                                    \
               Example: executor-pulse every 2h, chronicle-\n                                    \
               lint Sunday 09:00, WhatsApp sync every 15min.\n  \
             cosmon-daemon-supervisor          the night watchman. Long-running\n                                    \
             (daemons.toml)                    processes that must stay alive —\n                                    \
               Telegram bot, Emacs daemon, MCP servers.\n\n\
             Decision rule — does the command finish on its own?\n  \
             Yes, re-fire on a cadence               -> cosmon-scheduler\n  \
             No, runs forever, restart if it dies    -> cosmon-daemon-supervisor\n\n\
             Operator views: cs scheduler status / cs daemons list|status|logs|reload.\n\
             Shared kill-switch: 'touch ~/.cosmon/stand-down.lock' silences both.\n\n\
             SCHEDULER DRY-RUN OUTPUT — four row types, one per patrol:\n  \
             FIRE     patrol is due; dispatch would spawn the command now\n  \
             SKIP     gate rejected (disabled, kill-switch, not-due-yet,\n           \
               already-sunsetted, missing require_env var, …)\n  \
             SUNSET   a [patrol.sunset] convergence rule fired. Scheduler\n           \
               records sunset_decided_at, emits patrol.sunsetted, runs\n           \
               on_sunset hooks, then short-circuits to SKIP on every\n           \
               subsequent tick.\n  \
             INVALID  schema or cadence error (cron parse failure, cadence\n           \
               XOR violation). Exit code 3.\n\n\
             Auto-sunset for probes: attach [patrol.sunset] to stop a measurement \
             campaign on a statistical criterion (variance-threshold / sample-count \
             / operator-trigger-only) instead of a calendar timer. TSV shape and \
             hook flags (notify_telegram, write_chronicle_stub) are described in \
             docs/probes/sample-file-convention.md.\n\
             Canonical image: the chronicle 2026-04-19 'Deux métiers, \
             deux outils' (réveil vs. veilleur de nuit) and 2026-04-19 'Le gardien \
             des chiens, et le gardien des portes' (the supervisor-of-the-supervisor \
             is launchd — one plist for the watchman, N dogs under its care).\n\
             Future: dynamic service registration (today both layers are TOML-\n\
             edited; a planned design will let a service register itself via \n\
             event/API/molecule — see the internal design notes for the design space).\n\n\
             ARCHIVE — durable proof-of-work trail (ADR-030, see docs/archive.md):\n  \
             cs archive list                 every archived molecule, all months\n  \
             cs archive list --year 2026     scoped to one year\n  \
             cs archive show <mol>           manifest + artifact inventory (prefix ok)\n  \
             cs archive verify <mol>         recompute SHA-256 hashes (exit 1 if tampered)\n  \
             cs archive prune --dry-run      what [archive.retention] would delete\n  \
             cs archive prune                execute the retention policy\n\n\
             Terminal transitions (cs done / cs collapse / cs freeze / cs stuck) \
             populate '.cosmon/state/archive/YYYY/MM/<id>/' when [archive] enabled = true \
             in the project config. The archive outlives worktree teardown — a fresh \
             clone sees every merged molecule's canonical snapshot without running cs.\n\n\
             Worked example — verify the current month's archive in CI:\n  \
             cs archive list --year \"$(date +%Y)\" --month \"$(date +%m)\" --json \\\n    \
               | jq -r '.entries[].molecule_id' \\\n    \
               | xargs -I{} cs archive verify {}\n\n\
             LOOPS — three places an agent loop can live \
             (chronicle 2026-05-19 'Trois places, un nom différé'):\n  \
             L-Ext         outside cosmon. The loop runs in an external binary \
             cosmon spawns through a tmux pane;\n                \
             cosmon owns spawn / supervision / pane-signature / liveness, \
             nothing of the loop itself.\n                \
             Adapters today: claude, aider, codex, opencode. \
             SupervisionMode = TmuxPane, LoopOwnership = External.\n  \
             L-Native      inside cosmon. The loop runs in-process inside \
             'cs tackle' (cosmon-provider +\n                \
             cosmon-agent-harness); cosmon owns the FSM, tool dispatch, message \
             log, briefing seal.\n                \
             Adapters today: openai, anthropic, llama-cpp (with 'llama' as \
             legacy CLI alias).\n                \
             SupervisionMode = InProcess, LoopOwnership = Cosmon.\n  \
             L-Composite   above cosmon. N typed roles share a typed state \
             (RoleRoutingTable). Vocabulary, not a\n                \
             Rust enum yet — a meta-deliberation refused the \
             three-variant enum because\n                \
             Composite is set-level (a property of an aggregate) while \
             External and Cosmon are atom-level\n                \
             (a property per Adapter). Lives today in academy's meta-fleet, \
             barely instantiated cosmon-side.\n\n\
             A second axis, RuntimeOwnership, names *who runs the model server* \
             the loop talks to (ADR-104). It is orthogonal to LoopOwnership: \
             a Cosmon loop can talk to a Vendor server (openai, anthropic via \
             the Direct-API path) or to an Operated server (llama-cpp \
             in-process, vllm-mlx sidecar). The 2×2 grid:\n  \
             (Cosmon, Operated)    cosmon runs the loop, the operator can pull \
             the model-server's plug.\n                          \
             llama-cpp today (in-process FFI), vllm-mlx tomorrow (Path B \
             sidecar).\n  \
             (Cosmon, Vendor)      cosmon runs the loop, the model server is a \
             vendor cosmon merely consumes.\n                          \
             openai, anthropic today (Direct-API).\n  \
             (External, Vendor)    cosmon spawns an external CLI talking to its \
             vendor cloud.\n                          \
             claude, aider, codex, opencode today (tmux-pane subprocess).\n  \
             (External, Operated)  reserved — no shipped row yet (e.g. \
             self-hosted Codex driven by cosmon).\n\n\
             Per-Adapter axes are read from BUILT_IN_AXES at \
             crates/cosmon-core/src/spawn_seam.rs and validated at every \
             'cs tackle'. ADRs: ADR-099 (ValidatedAdapterName), ADR-101 \
             (SupervisionMode), ADR-103 (LoopOwnership), ADR-104 \
             (RuntimeOwnership).\n\n\
             ADAPTERS — canonical names and their per-Adapter axes \
             (registry projection, ADR-106 D2/D4):\n  \
             claude        (TmuxPane,  External, Vendor)    Claude Code CLI in \
             a tmux pane — vendor cloud.\n  \
             aider         (TmuxPane,  External, Vendor)    aider CLI in a \
             tmux pane — non-LLM coding agent.\n  \
             codex         (TmuxPane,  External, Vendor)    OpenAI Codex CLI \
             in a tmux pane — vendor cloud. Interactive steerable TUI by \
             default (whisperable, parity with claude); \
             [adapters.codex].mode = \"exec\" for the legacy fire-and-forget \
             'codex exec' batch path.\n  \
             opencode      (TmuxPane,  External, Vendor)    opencode \
             (sst/opencode) CLI in a tmux pane — vendor cloud.\n  \
             openai        (InProcess, Cosmon,   Vendor)    OpenAI chat-\
             completions HTTP, in-process loop (cosmon-agent-harness).\n  \
             anthropic     (InProcess, Cosmon,   Vendor)    Anthropic messages \
             HTTP, in-process loop (cosmon-agent-harness).\n  \
             llama-cpp     (InProcess, Cosmon,   Operated)  llama.cpp FFI \
             in-process — operator-run weights on operator hardware.\n  \
             llama         (InProcess, Cosmon,   Operated)  legacy CLI alias \
             of llama-cpp (ADR-106 D4) — emits the canonical name in events.\n  \
             local         (InProcess, Cosmon,   Operated)  Ollama OpenAI-compat \
             /v1 in-process loop — the built-in default, no Claude Code.\n  \
             ollama        (InProcess, Cosmon,   Operated)  canonical alias of \
             'local' (task-20260707-7d27) — '--adapter ollama' routes to the \
             same floor and stamps 'ollama' in events.\n\n\
             Selection happens at 'cs tackle' time. Resolution order:\n  \
             'cs tackle --adapter <NAME>'      → CLI flag wins.\n  \
             formula step 'adapter = \"<NAME>\"'  → per-workflow override \
             (e.g. a deep-think panel pins claude).\n  \
             $COSMON_DEFAULT_ADAPTER            → operator session hammer \
             (one export, everywhere, no committed config).\n  \
             '.cosmon/config.toml::[adapters.default]'  → per-galaxy \
             default for the project.\n  \
             '~/.config/cosmon/config.toml::[adapters.default]'  → global \
             machine-wide default (only when the per-galaxy config is silent).\n  \
             built-in 'local'                  → config-undeletable floor \
             (Ollama-backed, no Claude Code in the default path).\n\n\
             Every 'cs tackle' invocation — with or without the flag — emits \
             one 'adapter_selected' envelope to events.jsonl carrying \
             { adapter_name, selection_source \
             (cli|formula_step|env_var|config|global_config|default), \
             role_hint?, loop_ownership (cosmon|external) }. The cat-test \
             over the event log answers \"which Adapter ran for this \
             molecule?\" without parsing shell history (jq -c \
             'select(.type == \"adapter_selected\")'). The same envelope is \
             what 'cs demo' will reuse when its --adapter pass-through lands \
             — the demo verb \
             merely forwards the flag to 'cs tackle'; the dispatch site is \
             unchanged.\n\n\
             Discovery path: an unknown name on 'cs tackle --adapter \
             <UNKNOWN>' aborts with a typed 'AdapterNotFound' carrying the \
             available names — no silent fallback (godin's error-path \
             discoverability rule). The full registry projection is exposed \
             read-only via 'cs config adapters [--json]' (cs.adapters.list/v1 \
             envelope, ADR-106 D3).\n\n\
             Loud fallback: a local model hard-\
             failure (crash/oom/timeout/connection-refused) reaches a remote \
             oracle ONLY via 'cs tackle --adapter <REMOTE> --fallback-from-\
             local <CAUSE>'. That re-tackle mints a 'local_fallback' line in \
             the same atom as the 'remote_egress_opt_in' egress grant, so a \
             fallback can never be silent. There is no automatic in-loop \
             fallback; soft 'is the output good?' judgement is undecidable \
             (Rice) and is not a valid cause.\n\n\
             MODEL SELECTION (per-molecule, ADR-097). A model \
             pin is the sibling of the adapter axis, resolved fresh at every \
             'cs tackle' — strong is NEVER inherited, silence resolves cheap. \
             Resolution order:\n  \
             'cs tackle --model <ID>'          → CLI flag wins.\n  \
             formula step 'model = \"<ID>\"'     → per-workflow pin (does NOT \
             propagate across nucleation).\n  \
             $COSMON_DEFAULT_MODEL (else legacy $ANTHROPIC_MODEL)  → operator \
             session hammer.\n  \
             '.cosmon/config.toml::[adapters.<name>.default_model]'  → \
             per-galaxy default, SCOPED to the adapter.\n  \
             '~/.config/cosmon/config.toml::[adapters.<name>.default_model]'  \
             → global default (only when the per-galaxy config is silent).\n  \
             floor 'None'                      → cosmon pins NO model; the \
             adapter's own default applies (byte-identical to no pin — a \
             strong model is unreachable from silence).\n\n\
             The id is carried opaquely: cosmon does not validate that <ID> is \
             legal for the adapter — the backend rejects an invalid \
             (adapter, model) pair at launch (composition validation is C5). \
             The claude adapter carries the pin through the ANTHROPIC_MODEL \
             per-session closure-shadow at spawn (no shared-state mutation); \
             the Direct-API adapters take it above their config default_model.\n\n\
             MODEL BUDGET (fail-closed strong-dispatch ceiling, ADR-097). \
             Declare which ids are STRONG (expensive) per adapter and \
             cap strong dispatches per window — both opt-in:\n  \
             '[adapters.<name>].strong = [\"<id>\", ...]'  → the strong \
             cost-class set. FAIL-OPEN: an unlisted id is non-strong (cheap); \
             a cost-class annotation, NOT a validity table.\n  \
             '[model_budget] strong_dispatch_cap = <K>'  → at most K strong \
             dispatches per 'window_hours' (default 24); absent ⇒ no ceiling. \
             'on_overflow' = \"downgrade\" (default — drop to the safe floor) \
             or \"abort\" (refuse).\n  \
             The count is a fold over 'events.jsonl' (never a counter file); \
             the (K+1)th strong pin fails closed and mints a 'model_ceiling_hit' \
             event. SAFE-DEFAULT GUARD: strong is reachable ONLY from --model / \
             a formula-pin; a strong config/env default is dropped to the floor, \
             and a strong '[adapters.<name>].default_model' FAILS 'cs reconcile \
             --check' (config may only downgrade).\n\n\
             Name stability: 'llama-cpp' is the canonical CLI name for the \
             in-process llama.cpp FFI adapter; 'llama' is a legacy alias kept \
             at the CLI seam only (ADR-106 D4) — the persisted ProviderId \
             serialises as 'llama_cpp' with serde(alias = \"llama\"), and \
             AdapterSelected emits the canonical name even when the operator \
             typed the alias. The KebabRenameBait set { openai, llama } is \
             tolerated at input but excluded from the published ProtocolStable\
             Names list the TLA+ invariant I3 RegistryTruth reads \
             (docs/specs/CosmonDocHarness.tla).";

/// References footer appended after the long-help body.
pub const AFTER_LONG_HELP: &str = "REFERENCES:\n  \
             Operator handbook — run 'cs help guide' or read docs/handbook.md \
             for conceptual clarification on cosmon's lifecycle, DAG, tags, \
             and monitoring model.\n  \
             Help-text audit — an internal note records the per-command \
             rationale for the examples rendered here.";
