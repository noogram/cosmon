# ADR-076 — `cs security activate` binary posture toggle

**Status:** Proposed (2026-04-26)
**Decider:** Noogram, on cosmon-ward signal from `mailroom / task-20260425-e3cd`
**Origin:** jobs §6 (*Activabilité par configuration — un flag, une commande*) and forgemaster §5 (*deny.toml — mode `warn` lundi → `deny` jeudi*) of `delib-20260425-39c1` (mailroom panel, 2026-04-25). Responses reproduced under `/srv/cosmon/mailroom/.cosmon/molecules/delib-20260425-39c1/responses/`.

---

## 1. Context

The 2026-04-25 mailroom security panel framed the supply-chain
posture as a **binary toggle**, not a continuous dial. The panel
verdict was unanimous on this point: *« la sécurité graduable est
binaire — préparée ou active. Entre les deux, "partiellement active"
est l'état où la bretelle carton existe et personne ne le sait. »*

The motivation is operational, not theoretical:

- **Lundi 28/04** — the tenant-demo deployment for Dan lands. The
  three couches indépendantes (Tailscale ACL, WebAuthn câblage,
  cargo-deny / vet / audit gates in CI) must be **prepared** —
  reachable, observable in CI logs, but `severity = "warn"` so they
  don't block the deployment on a transitive yanked-crate finding.
- **Jeudi 30/04, 16h** — after operator enrollment of the YubiKey,
  the toggle flips to **active**: `warn → deny` everywhere, WebAuthn
  required. One commit, one push, CI runs strict.
- **Rollback** must be 30 seconds. If active mode breaks the
  deployment pipeline, `cs security activate --rollback` returns the
  surface to prepared without a redeploy.

Until this ADR landed, those transitions would have been hand-edits
across `deny.toml`, `.github/workflows/deny.yml`, and a future
WebAuthn config. Hand-edits are exactly the surface where partial
state lands silently — one file flips, the other doesn't, and the
operator now has a bretelle carton next to a bretelle béton.

## 2. Decision

Inscribe the binary posture as a first-class CLI verb:

```
cs security activate              # prepared → active (warn → deny)
cs security activate --rollback   # active → prepared (deny → warn)
cs security status                # show current mode + drift
```

**Operator-only**: refuses to run when `COSMON_MOL_DIR` is set in the
environment (worker context). The posture toggle joins the kill-switch
peer set: manual, non-overridable, first-class. A worker that mutates
posture by mistake silently weakens the whole system; the simplest
enforcement is to refuse the worker process altogether.

**Atomic**: a single `cs security activate` invocation rewrites
`deny.toml` and `.github/workflows/deny.yml` together, then commits
them as one git commit. Either both files flip or neither does. No
intermediate state survives a partial run.

**Idempotent**: the file-edit functions are designed to be safe under
re-application. Running `cs security activate` twice from `prepared`
state errors loudly the second time (the `detect_posture` precondition
catches it) rather than corrupting the deny.toml form.

**Recorded, not authoritative**: `~/.config/cosmon/security.toml`
caches the chosen mode for downstream readers (WebAuthn enforcement,
status displays). The on-disk gate files (`deny.toml`,
`.github/workflows/deny.yml`) remain the source of truth — a drift
between the cache and the gates is reported by `cs security status`
and resolved by re-running the toggle from a clean state.

### 2.1 Concrete file transitions

`deny.toml`:

| Field                    | prepared              | active                 |
| ------------------------ | --------------------- | ---------------------- |
| `[advisories].yanked`    | `"warn"`              | `"deny"`               |
| `[advisories].vulnerability` | (absent)          | `"deny"` (added)       |

`.github/workflows/deny.yml`:

| Step                   | prepared                          | active                  |
| ---------------------- | --------------------------------- | ----------------------- |
| `cargo vet --locked`   | suffixed with `\|\| true`         | bare (no passthrough)   |

`~/.config/cosmon/security.toml`:

```toml
[posture]
mode = "active"           # or "prepared"
updated_at = "<rfc3339>"

[webauthn]
required = true           # mirrors posture: true on active, false on prepared
```

### 2.2 What the command does NOT do

- **Does not enforce WebAuthn.** That belongs to tenant-demo /
  cosmon-cockpit-http when it lands; the `webauthn.required` flag in
  security.toml is a hint they read.
- **Does not touch Tailscale ACLs.** The panel verdict was *« ACL
  DURE dès samedi (zéro coût opérateur) »* — that is a one-shot
  manual operation outside cosmon's reach.
- **Does not gate the local `cargo build`.** The posture is enforced
  in CI (workflow + deny.toml) and at the operator gate (lefthook).
  The build itself stays fast.

## 3. Why operator-only — the kill-switch parallel

The `Operator revocation` clause of the mailroom kill-switch
catalogue (THESIS) is *manual, non-overridable, first-class*. The
security posture toggle carries the same flavour: an operator gesture
that lifts the system from `prepared` to `active` (or back) must not
be reachable through any worker code path. Otherwise an
adversarial-input worker could either *(a)* silently rollback to
prepared and mask supply-chain findings, or *(b)* prematurely activate
and break the ongoing deployment.

The `COSMON_MOL_DIR` env-var check is not a security boundary in the
cryptographic sense — a malicious worker could in principle unset it.
The point is *separation of concerns*: a well-behaved worker has no
plausible reason to invoke `cs security activate`, so refusing it
forces the operator to be the explicit subject of every posture
transition. The audit trail in `git log` then has one author per
transition: the operator.

## 4. Alternatives rejected

- **Hand-edit deny.toml + workflow.** The status quo. Re-fails the
  binary invariant — partial state is one tab-completion away.
- **CI-side flag (env var, GitHub Actions input).** Drifts away from
  the on-disk record. The operator now has to remember which env var
  is set on which workflow run; the audit trail moves from `git log`
  to GitHub UI clicks.
- **A new `cosmon-security` crate.** Premature. The posture toggle is
  ~600 lines including tests; the file edits are local to the
  cosmon-cli. Promoting to a crate when the second consumer appears
  (e.g. when tenant-demo needs to read the same security.toml) is the
  right time.
- **Continuous severity dial.** The panel rejected this explicitly
  (jobs §6, *« partiellement active est l'état où la bretelle carton
  existe et personne ne le sait »*). Two states only.

## 5. Audit & rollback

- The auto-generated commit message names the transition direction
  (`security: posture prepared → active` or the reverse). Searching
  `git log --grep="security: posture"` enumerates every flip.
- Rollback is one command, target ≤ 30 seconds: `cs security activate
  --rollback`. The CI workflow re-runs on the next push and returns
  the gates to warn-mode within one CI cycle.
- Workers in flight are not interrupted. The toggle modifies only
  CI-side artifacts and the operator's local config — running tackle
  workers continue under the posture in effect at their spawn time.

## 6. Open questions (deferred)

- **Multi-repo activation.** Today `cs security activate` operates on
  one repo (auto-detected from CWD or `--root`). When the tenant-demo and
  noogram repos adopt the same posture model, a `cs security activate
  --all` aggregator becomes useful. Not a blocker for v1.
- **Posture history file.** `security.toml` stores only the current
  mode. If the audit trail in `git log` proves insufficient (e.g. for
  compliance review), append a `.cosmon/state/security/history.jsonl`
  with one record per transition. Defer until a concrete requirement.
- **Notary attestation of the posture flip.** The current commit is
  unsigned. If the operator's notary key (ADR-056) becomes the de
  facto identity for security-relevant gestures, the commit message
  could include a `cs notarize`-style attestation. Defer until ADR-056
  graduates from Proposed.

## 7. References

- `delib-20260425-39c1` — mailroom security panel synthesis
  (`/srv/cosmon/mailroom/.cosmon/molecules/delib-20260425-39c1/synthesis.md`)
- jobs response §6 — *Activabilité par configuration — un flag, une
  commande* (`responses/jobs.md`)
- forgemaster response §5 — *deny.toml — mode `warn` lundi → `deny`
  jeudi* (`responses/forgemaster.md`)
- ADR-022 (mailroom) — Silence-on-Expected-Signal kill-switch (the
  binary-toggle reasoning lives in the same family)
- ADR-075 — Oracle Boundary for `cs tackle` (peer cosmon-ward
  inscription from the same panel)
- THESIS (mailroom) §Kill-Switch — `Operator revocation` clause,
  the structural ancestor of operator-only gestures
