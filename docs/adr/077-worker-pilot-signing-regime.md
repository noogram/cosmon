# ADR-077 — Régime de signature workers/pilot

**Status:** Proposed (2026-04-26)
**Decider:** Noogram, on cosmon-ward signal from `mailroom / task-20260425-40a2`
**Origin:** convergence C4 of `delib-20260425-39c1` (mailroom security panel, 2026-04-25) — forgemaster §3 *Signed commits + worker auto-pilote* and turing *Verdicts compacts* (*« workers signent jamais. Push humain ou push CI-OIDC. Sans exception. »*). Responses reproduced under `/srv/cosmon/mailroom/.cosmon/molecules/delib-20260425-39c1/responses/`.

---

## 1. Context

The cosmon worker fleet (`cs tackle`, autonomous panels, runtime DAGs) commits autonomously into per-task worktrees at high frequency. Two simultaneous pressures converge on the same surface:

- **Branch protection on `main` is becoming mandatory.** The 2026-04-25 mailroom security panel adopted *« un seul maillon faible suffit »* as red-line; one of the load-bearing controls on `main` is `require signed commits`. Without it, a stolen GitHub PAT or a compromised local laptop can fast-forward `main` and the audit trail loses the *"who actually pushed"* anchor.
- **The auto-pilot session mode runs workers in background.** Workers spawn tackle processes that produce dozens of commits per hour without operator presence. Any signing scheme that requires a per-commit human gesture (YubiKey-touch on every commit, password-protected GPG passphrase per commit) silently kills auto-pilot and the operator only notices days later when the molecule queue stalls.

Until this ADR landed, the four candidate regimes (raw filesystem GPG key, pilot-signs-on-merge, deferred-signing-with-review, mandatory YubiKey-touch-per-commit) were unranked. Workers in flight today produce unsigned commits on worktree branches; nothing prevents a future hand-edit from accidentally provisioning a GPG key on disk and *technically* satisfying the upcoming branch-protection rule while breaking the threat model the rule exists to enforce.

The decision below picks the regime, names the alternatives explicitly rejected, and inscribes the operational consequences so future workers and operators inherit the same envelope.

## 2. Decision

Adopt the **pilot-signs-the-merge** regime (forgemaster §3 option (b)):

1. **Workers commit unsigned on worktree branches** (`feat/task-XXXX`, `feat/delib-YYYY`, `feat/task-YYYYMMDD-HHHH`). Workers never write `.git/config` signing keys, never invoke `git commit -S`, never receive a GPG/SSH-FIDO secret on disk.
2. **Workers never touch `main` directly.** All paths to `main` go through a pull request that the operator (the *pilot*) reviews and merges; the merge commit is the only commit on `main` whose authorship matters for the audit trail.
3. **Branch protection on `main` requires signed commits, PR before merge, status checks, and blocks force-push.** The signature requirement applies *only on `main`* — worktree branches remain unsigned.
4. **The pilot signs the merge commit.** Push to `main` happens by one of two paths, both auditable:
   - **Pilot push.** The operator pushes from a workstation with a YubiKey-touch SSH/GPG signing key. The merge commit carries the operator's signature.
   - **CI-OIDC push.** A GitHub Actions workflow with `id-token: write` produces a signed merge attestation via `actions/attest-build-provenance`; the workflow itself runs on a tag/branch the operator authorized. No third path.
5. **`cs done` runs under the pilot, not the worker.** The terminal cosmon verb (close molecule, append final state, push) is operator-only. Workers terminate by writing their final state into the worktree and exiting; the operator runs `cs done` after review.

The auto-pilot session mode (`~/.claude/CLAUDE.md` §Auto-pilot) is **not broken** by this regime. Workers continue to run in background producing unsigned commits on their own branches; the operator's involvement is bounded to the merge gesture, which already required review under the auto-pilot rules (*"Will continue to ask atomically: destructive operations — git push"*).

### 2.1 What changes on disk

`.github/branch-protection-rules` (or the equivalent GitHub UI state) for `main`:

| Rule                              | Value                          |
| --------------------------------- | ------------------------------ |
| Require pull request before merge | **on**, 0 approvals (solo)     |
| Require signed commits            | **on**                         |
| Require status checks to pass     | `deny.yml`, `ci.yml`           |
| Allow force-push                  | **off**                        |
| Allow deletion                    | **off**                        |
| Restrict who can push             | operator + GitHub Actions only |

Worker invariants (enforced by `cs tackle` and worker harness, not by branch protection):

- `git config --local commit.gpgsign` is **never set** in worker worktrees.
- The worker harness refuses to start if `~/.gnupg/` or `~/.ssh/id_ed25519` (or any FIDO-resident credential) is mounted into the worker's accessible filesystem with a private key in the clear.
- The worker harness refuses to invoke `git push` on its own — push is operator-only (peer of the `cs security activate` operator-only gate, ADR-076).

### 2.2 What this regime does **not** do

- **Does not require YubiKey-touch on every commit.** That alternative (option (d), §3) was explicitly rejected as auto-pilot-incompatible.
- **Does not require a per-worker signing key.** No GPG/SSH-FIDO key, no Vault-issued ephemeral key, no key-wrap scheme. Workers stay key-less.
- **Does not block local hand-edits on `main`.** The branch protection rule forbids unsigned pushes, not unsigned local commits — the operator can `git commit` unsigned locally and then re-author the merge with `git commit -S --amend` before pushing. The protection runs at the push boundary, not the commit boundary.
- **Does not change the cosmon notary protocol** (ADR-056, ADR-059, ADR-060). Notary seals (`cs notarize`) live in their own canonical form with their own signing scheme; git commit signatures are an orthogonal channel governing source-of-truth integrity, not commitment integrity.
- **Does not gate the worker's `cargo build`.** The signing surface is at push time, not at build time.

## 3. Alternatives rejected

### (a) GPG key on disk in the worker worktree

The worker holds a private signing key not protected by hardware touch. Each `git commit` in the worktree is signed automatically.

**Rejected.** The key file is readable by any process the worker spawns — including LLM sub-agents that might exfiltrate it (turing's *Oracle Boundary* failure mode). A compromised worker becomes a key-emission machine. The signature ceases to mean *"the operator stands behind this"* and starts to mean *"some process inside the worker's filesystem ran ed25519-sign at some point"* — which is the exact signal-loss the panel rejected.

### (c) Deferred signing with explicit review window

Same as the chosen (b), but with a named latency window (e.g. "all worktree commits batch-sign at end of day"). Operationally identical to (b) once the worker→pilot handoff happens at merge time; the explicit window adds a queue without changing the trade-off. Folded into (b).

### (d) YubiKey-touch on every commit

The signing key is hardware-resident; every commit requires a physical touch on the YubiKey.

**Rejected.** Bocks auto-pilot completely. A worker producing 30 commits/hour during a long backtest molecule would require the operator to be physically present at the laptop tapping the YubiKey 30 times per hour — the operating mode this entire architecture exists to avoid. The security gain is real (no signature without physical presence) but it lands at the wrong layer: the threat model doesn't need every commit signed, it needs every *push to `main`* signed. (b) collapses the surface to exactly what needs the touch.

### (e) Worker-local ephemeral key issued by a Vault

A Vault (HashiCorp Vault, AWS Secrets Manager, custom Custody Vault) issues short-lived signing keys to workers on demand; the keys expire after N minutes.

**Rejected for now.** Premature. Adds a new dependency (Vault), a new surface to compromise (Vault auth), and a new failure mode (Vault unavailable → workers stall) for a benefit (per-commit signatures) that (b) already buys at the only layer where it matters (the merge to `main`). Re-evaluate when ADR-060's Custody Vault lands and the cosmon notary and git-signing surfaces converge on the same key-management substrate. Until then, (b) is strictly simpler.

## 4. Operational consequences

### 4.1 `cs done` becomes operator-gated

Today `cs done` can be invoked by anyone with the molecule context. Under this regime, `cs done` on a molecule that touched `main` (or proposes a merge to `main`) MUST be invoked by the operator. The implementation enforcement parallels `cs security activate` (ADR-076 §3): refuse to run when `COSMON_MOL_DIR` is set in the environment, i.e. when the caller is itself a worker. A worker-generated `cs done` becomes the worker writing a *"done"* marker in the worktree state; the operator's `cs done` is the terminal authoritative close.

This is a behavioral change, not a code change in this ADR. The next molecule that touches the cosmon-cli `cs done` surface picks up the gate.

### 4.2 Auto-pilot is preserved

Workers continue to run in background producing unsigned commits on worktree branches under auto-pilot. The operator's role under auto-pilot remains unchanged from `~/.claude/CLAUDE.md` §Auto-pilot:

- *Will auto-do:* close completed molecules locally, tackle unblocked `temp:hot` molecules, etc. — none of these require pushing to `main`.
- *Will continue to ask atomically:* destructive operations including `git push`. The merge to `main` falls in this set; the operator presence at merge time is the existing protocol, not a new burden.

### 4.3 No GPG/SSH-FIDO secret on worker disk — ever

This is an invariant, not a recommendation. The worker harness MUST refuse to start if it detects a usable signing key in its accessible filesystem. The mechanism (env-var check, scan of `~/.gnupg/`, refuse-on-detection) is for the next worker-harness ADR; the principle is fixed here.

### 4.4 Push happens via two paths, both auditable

- **Operator push from workstation.** Workstation holds the YubiKey + signing config. `git push` from the workstation is signed; `git log --show-signature` on `main` shows the operator key.
- **CI-OIDC push.** GitHub Actions with `id-token: write` produces a build provenance attestation (forgemaster §2 SLSA L2 path). The push to `main` from CI carries the OIDC-signed merge.

Both paths terminate in a `git log` on `main` whose every commit has a verifiable signature. The third path (worker pushes directly) is forbidden — branch protection refuses it, and the worker harness refuses to attempt it.

### 4.5 `git log --show-signature` is the audit surface

After this ADR lands and branch protection is configured, the audit trail for *"who authorized this state of `main`"* is exactly:

```
git log --show-signature main
```

Every commit on `main` shows either *"Good signature from Noogram"* or *"Good signature from GitHub Actions"*. Anything else is a protocol violation and the merge would have been refused at push time. There is no fourth signer.

## 5. Why operator-only push — the kill-switch parallel

The `Operator revocation` clause of the mailroom kill-switch catalogue (THESIS) and the `cs security activate` operator-only gate (ADR-076 §3) form a peer set with this ADR's push gate: three operator-only gestures that together define the irreversibility envelope of the system.

- **Operator revocation** — *manual, non-overridable, first-class*.
- **Security posture toggle** — *operator-only via `COSMON_MOL_DIR` refusal*.
- **Push to `main`** — *operator gesture (workstation YubiKey) or operator-authorized CI workflow (OIDC). No third path.*

Each of these would be a single point of compromise if a worker code path could reach it. The pattern is the same: refuse the worker process altogether at the surface, and the audit trail in `git log` (or `~/.config/cosmon/security.toml`, or `THESIS.md`) acquires one author per gesture: the operator.

## 6. Audit & rollback

- The branch-protection rule is reversible from the GitHub UI by the operator. If the regime breaks (e.g. CI-OIDC push fails for a deploy-critical patch), the operator can temporarily disable *Require signed commits* on `main`, push the patch, and re-enable. Target rollback time: ≤2 minutes via GitHub UI.
- The worker harness invariants (no `commit.gpgsign`, no signing key on disk, no `git push`) are enforced in code; their failure mode is a refusal to start, not a silent bypass. A worker that cannot start surfaces immediately in the operator's `cs ps` view.
- Past unsigned commits on worktree branches remain valid forever. They simply cannot be fast-forwarded onto `main` without going through the merge gate. The history is preserved; only the *authority surface on `main`* is restricted.

## 7. Open questions (deferred)

- **Multi-operator pushes.** Today the operator is one human (Noogram). When witnesses-with-technical-access land (delib-20260425-39c1 §C5: YubiKey-postée obligatoire dès le 1er témoin externe avec accès technique), the *Restrict who can push* list grows by one entry per witness. The promotion path (witness onboarding → push permission) needs its own ADR; this one fixes the regime for the solo case.
- **Operator key rotation.** When the operator rotates the YubiKey signing key, every prior commit on `main` was signed by the old key and remains valid forever (`git verify-commit` against archived public keys). The rotation event itself needs a notary-equivalent surface for git commit signing keys. Defer to ADR-060 (`cs rotate-key`) when the Custody Vault lands; the git-signing-key rotation may share that substrate.
- **Cross-galaxy push.** The same regime applies to every cosmon-managed repo (cosmon, mailroom, tenant-demo, showroom, sandbox, cadence, …). A `cs security activate-branch-protection` aggregator would let the operator flip the rule across all repos in one gesture. Not a blocker for v1 — initial activation is per-repo via GitHub UI.
- **Local commits on `main` by the operator.** When the operator hand-edits `main` (e.g. `chore(state): track artifacts for task-XXXX` commits visible in the recent history), each such commit is signed by the operator's local YubiKey. The branch-protection rule covers this transparently. No special case needed; documented here for completeness.
- **Worker harness enforcement code.** The actual code that refuses to start a worker when a signing key is on disk is not in this ADR. A follow-up molecule wires the check into the worker harness; this ADR fixes the invariant that check enforces.

## 8. References

- `delib-20260425-39c1` — mailroom security panel synthesis (`/srv/cosmon/mailroom/.cosmon/molecules/delib-20260425-39c1/synthesis.md`), C4 *Workers ne signent jamais ; pilot signe le merge sur main*
- forgemaster response §3 — *Signed commits + worker auto-pilote* (`responses/forgemaster.md`)
- turing response *Verdicts compacts* — *« Auto-pilote : workers signent jamais. Push humain ou push CI-OIDC. Sans exception. »* (`responses/turing.md`)
- ADR-076 — `cs security activate` binary posture toggle (peer cosmon-ward inscription from the same panel; this ADR shares the operator-only gating pattern)
- ADR-075 — Oracle Boundary for `cs tackle` (the worker-harness substrate this ADR's invariants extend)
- ADR-056 — Notary Protocol v0 (orthogonal signing surface; cosmon-notary signs commitments, this ADR signs git history; both stay distinct)
- ADR-049 — Cosmon-ward feedback flow (the meta-protocol under which this ADR was raised)
- ADR-052 — One ledger, one writer, one witness (the *one writer* on `main` is the operator under this regime)
- THESIS (mailroom) §Kill-Switch — `Operator revocation` clause, the structural ancestor of operator-only gestures
- `~/.claude/CLAUDE.md` §Auto-pilot — the session mode this regime preserves intact

## 9. Governance

If this ADR and the branch-protection configuration disagree, **the ADR wins** — file a bead to fix the configuration. If this ADR and the worker harness code disagree, **the ADR wins** — file a bead to fix the code.

When the first merge to `main` lands under the regime defined here (either operator-pushed with YubiKey signature, or CI-OIDC-pushed with attestation), a chronicle entry belongs in the internal chronicles: *« le pilote signe la fusion, pas chaque coup de stylo »*. The spirit: the discipline scales with the surface it actually protects, not with the surface it is theoretically applicable to.
