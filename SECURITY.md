# Security Policy

## Reporting a vulnerability

Email **security@noogram.org** with a description, reproduction steps, and the
affected version or commit. Please report privately first — do not open a public
GitHub issue for a suspected vulnerability. We aim to acknowledge a report within
a few business days and will coordinate a fix and disclosure timeline with you.

If you prefer encrypted mail, say so in a first low-detail message and we will
exchange a key.

## Threat model — read this before you run a fleet

Cosmon is an orchestrator for **AI coding agents that run real shell commands on
your machine**. That is the product, and it shapes the trust model. Be candid
with yourself about what you are running:

- **The agent harness runs an unconfined shell.** The `exec_command` tool
  (`crates/cosmon-agent-harness/src/tools/exec_command.rs`) owns a long-lived
  `/bin/bash`. It has **no `chroot`, no mount namespace, and no read
  allowlist** — `cd /`, absolute paths, and `$HOME` all resolve to the real
  filesystem. **The worktree is *not* a security boundary for this tool.** A
  command the agent runs can read anything your uid can, including secrets
  outside the worktree (`~/.ssh/id_rsa`, an OIDC bearer store at
  `~/.config/cosmon-remote/credentials/`). The path-based tools
  (`read_file` / `write_file` / `list_dir`) *are* pinned under the worktree via
  `sanitize_join`; the shell tool is the deliberate hole they are not.

- **The only enforced boundary is the egress network namespace.**
  `cosmon_core::egress` (see `crates/cosmon-core/src/egress.rs`) can put the
  harness in a `StrictLocal` posture that makes an outbound remote-oracle
  shellout a *refused syscall*. It blocks the **wire only** — it does **not**
  confine filesystem reads, and a secret read on-box still exfiltrates through
  the data plane (agent output → `synthesis.md` → git branch →
  operator/downstream). Under the **default `AllowAll` posture even the wire is
  open.**

- **Repo-supplied shell is trust-gated (`cs trust`).** Cosmon runs shell
  strings the *repository* supplies — a formula's `command` /
  `verification.criteria` steps (`crates/cosmon-cli/src/cmd/evolve.rs`,
  `cmd/tackle.rs`, `cmd/verify.rs`) and the `post_merge` / `pre_done` hooks in
  `.cosmon/config.toml` (`cmd/done.rs`). To stop a freshly-cloned hostile repo
  from executing code the moment you tackle or merge it (RCE-by-clone), those
  `sh -c` sites are gated on a one-bit, per-repository trust grant recorded
  **outside** the repo (`~/.cosmon/trust/`, overridable with
  `COSMON_TRUST_DIR`). Until you vouch for a repository once with `cs trust`,
  cosmon **refuses** to run its formulas and hooks; editing the shell surface
  (`.cosmon/config.toml` or a formula) revokes the grant until you re-`cs trust`
  it, exactly as editing `.envrc` revokes a `direnv allow`. CI can bypass the
  prompt with `COSMON_ASSUME_TRUSTED=1` for a repo it vetted out-of-band. This
  is a *trust* gate, not a *sandbox*: once you trust a repo, its shell runs
  under the same unconfined model described above. The hashed shell surface is
  **transitive one hop**: it folds in not just `config.toml` and the formulas
  but every *delegated target* they reference — a `bash scripts/deploy.sh`
  hook, a `build_command = "python ci/build.py"` gate, a `make` gate's implicit
  `Makefile` — resolved language-agnostically and *jailed* to the repository
  root. Rewriting a delegated script (not just the pointer) revokes the grant;
  a referenced file that exists but cannot be read folds a distinct fail-closed
  sentinel rather than hashing as empty. That transitive hop follows references
  found in **shell-bearing values only**: prose fields (`description`,
  `acceptance`, `title`, …) and TOML comments cannot reach `sh -c`, so a formula
  that merely *mentions* `README.md` in its documentation does not enlist the
  real `README.md`. The narrowing costs no coverage — prose cannot inject shell,
  and `config.toml` and the formulas are still hashed byte-for-byte, so editing
  even a comment still revokes the grant. It is a denylist, not an allowlist: an
  unrecognized key counts as shell-bearing, and a surface file that does not
  parse as TOML falls back to a full-text scan.

- **The tenant installer trusts the network.** The hosted `install.sh` path for
  `cosmon-remote` fetches a binary over the wire; treat it as `curl | sh` and run
  it only on a host and network you trust, or build from source instead.

The governing stance, stated plainly: **cosmon runs code you would run
yourself.** Point it only at repositories and models you trust, on a machine
whose blast radius you accept, ideally a dedicated/sandboxed user or VM. Adding
a mount-namespace / chroot / read-allowlist to close the filesystem escape is a
tracked, operator-owned decision, not a shipped guarantee.

## What is in scope

- Privilege escalation or sandbox escape **beyond** the documented "unconfined
  shell" model above (e.g. a path-based tool escaping the worktree, or an egress
  bypass of a `StrictLocal` posture).
- State-store corruption or crash-recovery paths that let a malicious molecule
  or crafted `state.json` execute code or destroy unrelated data.
- Supply-chain gaps in the release/install pipeline (unpinned actions, missing
  signature verification a user is told exists, dependency confusion).
- Confidentiality-gate bypasses that leak operator/fund identity onto the
  publishable surface.
- A bypass of the repo-supplied-shell **trust gate**: running a cloned
  repository's formula `command` / `verification` step or its `post_merge` /
  `pre_done` hook via `sh -c` **without** a `cs trust` grant (or the documented
  `COSMON_ASSUME_TRUSTED` opt-in) — e.g. an in-tree marker that forges trust, or
  a shell-surface mutation that does not invalidate an existing grant. This
  explicitly includes **rewriting a delegated script** a trusted `config.toml` /
  formula points at (`bash scripts/x.sh`, `python ci/build.py`, a `make` gate's
  `Makefile`) without the grant going stale.

## What is out of scope

- The unconfined agent shell reading local files under the default trust model
  described above — that is documented behaviour, not a vulnerability. If you can
  make it read files **outside** what the running uid should reach, that *is* in
  scope.
- Findings that require an already-compromised host or a malicious model backend
  the operator deliberately configured.

## Supported versions

Cosmon is pre-1.0 and moves fast. Security fixes land on `main`; there is no
back-port channel yet. Run a recent build.
