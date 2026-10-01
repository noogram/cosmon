# Remote harvest authority — contract

**Status:** Ratified by work unit W0 of issue #120 (2026-09-29).
W4 implements the remote policy resolver, dedicated scope, binding-only
source rule, and trusted route-to-effect transport. W5–W7 add the typed
refusals, administrative routes, and operator-side commands. The opt-in
container acceptance cases remain for W9.
Absent `remote` configuration retains the legacy behavior.

**Amends:** [ADR-172](../adr/172-done-authority-is-an-operator-sealed-capability.md)
(amendment of 2026-09-29), [ADR-080](../adr/080-remote-pilot-port-https-oidc.md)
§5.4 and new §6.6, [ADR-176](../adr/176-remote-harvest-authority-is-a-sealed-capability.md)
(amendment of 2026-09-29), and the §3 and §8p paragraphs of
[`architectural-invariants.md`](../architectural-invariants.md).

**Acceptance cases:** `tests/e2e/test_harvest_profiles.py`, opt-in (§14).

---

## 1. The problem this contract answers

Before this contract, remote `done` (`POST /v1/molecules/{id}/done`) needed
`cosmon:molecule:write`, `[harvest_authority] required = true`, and a valid
operator-signed grant. There was no supported production key and grant
issuance command. An external operator of the remote service could not
complete the journey with shipped tooling. The explicit profiles and
operator commands below close that usability gap while preserving the legacy
policy when `remote` is absent.

The seal has a real purpose — bounded authority that possession of the
ordinary API credential cannot create — so it is kept, as an explicit
stronger policy. A newly provisioned operator-owned service can instead
select a dedicated harvest scope and no seal. The choice is per tenant,
explicit, and never made for an operator by an upgrade.

## 2. Vocabulary

| Term | Meaning |
|---|---|
| **Harvest scope** | The OAuth scope `cosmon:molecule:harvest`. Required for remote `done` on an explicit profile. `cosmon:molecule:write` does not imply it, and it does not imply `write`. |
| **Remote policy** | `[harvest_authority] remote` in the tenant galaxy's `.cosmon/config.toml`, a closed enum: `"disabled"`, `"scoped"`, `"sealed"`. |
| **Explicit profile** | A galaxy whose `remote` key is present. |
| **Legacy state** | A galaxy whose `remote` key is absent. A compatibility state, not a fourth selectable profile. |
| **Local seal policy** | The existing `[harvest_authority] required`. It keeps governing the local `cs done` and runtime paths exactly as today, and is also the legacy compatibility input. |
| **Policy provenance** | `Explicit` or `Legacy`, returned by the resolver beside the effective policy, and reported in status and audit. |

## 3. Policy resolution — every configuration has one decision

A pure resolver maps the configuration to one of the decisions below. The
effective policy is re-resolved at the route and again under the effect lock
(§6); it is never cached across the two.

| # | `remote` | `required` | Effective remote policy | Scope condition at the route | Seal condition at the effect |
|---|---|---|---|---|---|
| R1 | absent | `false` or absent | **Disabled** (legacy) — today's behaviour | none enables it | — |
| R2 | absent | `true` | **Legacy sealed** | `cosmon:molecule:write` **or** harvest scope | valid installed grant, as today |
| R3 | `"disabled"` | any | **Disabled** (explicit) | none enables it, even with a valid grant | — |
| R4 | `"scoped"` | `false` or absent | **Scoped** | harvest scope alone; `write` is neither necessary nor sufficient | none — no key, no grant |
| R5 | `"scoped"` | `true` | **Configuration conflict** — refuse | — | — |
| R6 | `"sealed"` | `false` or absent | **Sealed** | harvest scope | valid grant |
| R7 | `"sealed"` | `true` | **Sealed** | harvest scope | valid grant |
| R8 | unknown value, malformed or unreadable config | — | **Configuration fault** — refuse | — | — |

Clarifications that are part of the table:

- **R2** is the only row where `write` still admits. It is the compatibility
  path for existing sealed deployments whose clients hold write plus a seal.
- **R5** refuses rather than silently weakening the local hardening that
  `required = true` expresses. Moving an existing sealed galaxy to `scoped`
  means writing `remote = "scoped"` **and** resolving `required` in the same
  administrative change; the remote command never alters local policy.
  `harvest configure --policy scoped` against a galaxy with
  `required = true` is therefore refused `harvest_policy_conflict` and writes
  nothing; R5 remains the decision for a configuration edited by hand.
- **R8** never defaults to an unsealed state. An unreadable configuration is a
  fault (`503`), not a disabled galaxy and not a scoped one.
- `cosmon:worker:spawn` is still required, in addition, for a request that
  arms auto-propel or retries (ADR-176 D6). The harvest scope does not imply
  it.
- The remote policy never affects the local `cs done` or the resident
  runtime. Those keep reading `required` exactly as today.

## 4. Upgrade and the end of legacy compatibility

- **No clock.** Legacy state has no sunset date and no automatic cutover. It
  ends only when an administrator explicitly writes `remote` for that galaxy.
- **Upgrade writes nothing.** `cs init --upgrade`, an adapter restart, image
  initialisation finding an empty directory, and a binding reload never write
  `remote`, never select `scoped` by absence of configuration, and never add
  the harvest scope to an already materialised binding.
- **Visible.** Status and audit report legacy provenance, so an operator can
  see which galaxies still rely on it.
- **Fresh provisioning** means a deliberate administrator action: the
  documented runbook writes `remote = "scoped"` and deliberately includes the
  harvest scope in the selected binding. The policy write and the scope write
  are separately observable; a partially provisioned tenant keeps refusing,
  and status names the missing step.

## 5. Sensitive options on explicit profiles

`HarvestOptions` (`crates/cosmon-core/src/harvest_door.rs`) keeps its full
parameter set on the wire (ADR-176, D4 reversal). What changes is which
options an explicit profile accepts.

| Option | Legacy state (R2) | Explicit `scoped` / `sealed` |
|---|---|---|
| `reason`, `strategy`, `if_completed`, `no_merge`, `no_worktree_remove`, `no_branch_delete`, `no_kill`, `no_auto_propel`, `propel_message`, `max_retries` | unchanged meaning | unchanged meaning (spawn scope still needed where ADR-176 D6 requires it) |
| `force` | unchanged (D4 parity) | refused `harvest_override_requires_ratification` |
| `skip_pre_done_hook` | unchanged (D4 parity) | refused `harvest_override_requires_ratification` |
| `deploy_off_trunk` | unchanged (D4 parity) | refused `harvest_override_requires_ratification` |
| a molecule carrying a reserved tag (ADR-172 D1) | refused on the remote door, as today | refused on the remote door, even if a grant names the reservation |

The refused options stay refused until an exact-override ratification format
exists (§9). Neither the harvest scope nor a v1 grant signs them. The remedy
named in the refusal is the operator's review and local workflow, never
"mint any grant". No option is silently ignored.

Legacy seals do not bind options either; status and documentation say so
during migration. This is the documented legacy behaviour, retained on
purpose — the operator decision of 2026-09-29 chose not to tighten the
legacy profile now, so W0 records no compatibility break.

## 6. Where authority is checked

1. **Route** (`routes/molecules.rs::done_molecule`). Identity and tenant
   admission run first, and the tenant comes from the exact
   issuer/subject/audience binding. The harvest scope is then checked against
   that tenant's resolved policy (with the R2 exception). These checks precede
   every success branch, including retry, `if_completed`, `no_merge` and the
   idempotent replies.
2. **Existing gates keep their meaning.** Reason, backlog, reservation,
   protected-path and merge gates are unchanged. Scope-only authority cannot
   clear a reservation or turn a gate refusal into success.
3. **Route to effect.** The server builds a typed remote admission value and
   carries it through `HarvestEffectPort` and `LibraryHarvestEffect` to a
   distinct remote entry of the shared harvest transaction. It binds tenant,
   galaxy, molecule, action, request options, authority source, expiry and
   policy provenance. It is not a wire field, CLI flag, serialized grant or
   environment variable.
4. **Effect, under the trunk lock.** The transaction reloads the
   authorization facts, resolves the remote policy again, and rechecks scope
   eligibility through an injected admission-validation port (expiry,
   revocation, binding change). A changed tenant, target, option set or policy
   refuses; a policy change needs a fresh request. The scoped arm then
   authorizes the ordinary effect directly. The sealed arm runs seal
   validation and consumption. Neither arm manufactures a
   `DoneAuthorization` or reaches "not in force" by setting `required = false`.
5. **Closure without merge** still needs remote policy and scope, checked
   immediately before the closure mutations, and spends no seal.

Domain decisions stay in `cosmon-core` and read no file, clock, environment,
HTTP or Git. Rust visibility prevents accidental misuse inside cooperating
code; it is not a boundary against arbitrary same-uid code (ADR-172 D5).

## 7. Executor support

| Executor | Legacy state | Explicit `scoped` / `sealed` |
|---|---|---|
| `LibraryHarvestEffect` (default) | supported | supported |
| explicitly configured `cs` binary | existing behaviour kept | refused `harvest_effect_unsupported` (`501 harvest_effect_unavailable`); remedy: remove the explicit binary selection |

No bypass flag or environment variable is ever sent to an old binary. New
sealed provisioning also selects the library, so both explicit profiles have
full structured diagnostics.

## 8. Custody of signing and trust administration

- **Sealed policy.** The signing key is encrypted, on an operator device
  independent of the service, used through an external signer. The service,
  the worker and the unattended beneficiary receive no private key material
  and expose no signing operation. Operator-side cosmon tooling
  (`cosmon-remote`) may generate the key through the external signer,
  construct challenges and orchestrate signing; it writes no custom
  production cryptography and never reuses the test-kit seed.
- **Default operator key location:**
  `$XDG_CONFIG_HOME/cosmon/harvest/keys/<profile>/<key-id>.key`
  (`~/.config` when XDG is unset), directory `0700`, file `0600`. Never inside
  a galaxy, worktree, service volume, grant directory, image layer, artifact
  output, profile TOML, argv or worker environment. A file mode is local
  hygiene, not proof of device isolation.
- **Grant expiry.** One hour by default. A grant without expiry needs the
  explicit `--no-expiry` selection.
- **Trust administration** (public key, policy, epoch) uses the existing
  disjoint admin credential (`AdminSeal`, `X-Cosmon-Admin-Token`) with
  compare-and-set against the expected prior state. A tenant bearer,
  including one holding the harvest scope, can never install or replace a
  key, select a policy or change the epoch. Where a deployment cannot enable
  the admin surface, the equivalent local operator command is supported and
  only public material is transferred; trust-root write is never added to a
  tenant scope.
- Theft of the admin credential is authority over the deployment and lies
  outside the narrower stolen-tenant-bearer guarantee.

## 9. Grant encoding

The v1 canonical bytes (`cosmon-harvest-grant-v1`) are unchanged, and
historical signed payloads are never re-serialized. Receipt identity is
normalized from the **signed** scope rather than from the unsigned outer
variant: molecule grants use their fingerprint, mission grants derive a
per-member identity. Readers accept the legacy receipt aliases, coalesce
equivalent entries and refuse conflicting ones. A versioned grant format is
introduced only if it carries exact override authorization (§5).

## 10. Recovery

An ambiguous recovery refuses `harvest_recovery_required` with durable
evidence and an operator reconciliation path. Cosmon never infers "landed"
from a receipt alone, and never erases a spend so that a request can retry.
The journal separates reservation of authority from integration and
finalization (W2 of the plan).

## 11. Typed refusals

The established top-level `error` label, HTTP status and door exit codes are
kept. A safe, additive `harvest_authorization` object carries the gate, a
stable reason and one action token:

```json
{
  "error": "not_authorized",
  "request_id": "req-example",
  "harvest_authorization": {
    "gate": "grant",
    "reason": "harvest_grant_missing",
    "action": "mint_grant"
  }
}
```

| Gate / reason | Status | Next gesture |
|---|---|---|
| `scope / harvest_scope_missing` | 403 | administrator issues `cosmon:molecule:harvest`; client refreshes credentials |
| `credential / harvest_credential_expired` | 403 | refresh the credential and retry |
| `credential / harvest_credential_revoked` | 403 | administrator reviews the revocation |
| `binding / harvest_binding_changed` | 403 | administrator inspects the current identity binding |
| `policy / harvest_disabled` | 403 `not_authorized` | administrator runs `harvest configure` |
| `policy / harvest_policy_conflict` | 503 | administrator resolves R5 |
| `key / harvest_key_missing` | 403 `not_authorized` | operator runs `harvest init` |
| `grant / harvest_grant_missing` | 403 `not_authorized` | operator runs `harvest grant` |
| `grant / harvest_grant_invalid`, `harvest_signature_invalid` | 403 | inspect status, import a correctly signed grant |
| `grant / harvest_grant_mismatch`, `harvest_grant_expired`, `harvest_grant_revoked` | 403 | inspect current facts, mint a matching grant |
| `facts / harvest_facts_changed` | 409 | retry after status |
| `facts / harvest_facts_unavailable` | 503 | administrator repairs state; no merge |
| `receipt / harvest_recovery_required` | 409 | reconcile durable evidence; never delete a receipt |
| `executor / harvest_effect_unsupported` | 501 `harvest_effect_unavailable` | remove explicit binary selection |
| `override / harvest_override_requires_ratification` | 403 | operator review / local workflow |

Unknown tenants and unmapped callers keep the existing admission rejection
without tenant details; a missing or foreign molecule keeps the existing
hiding boundary. No diagnostic carries a filesystem path, signed payload,
private key, candidate dump or another tenant's facts. Old clients see their
coarse labels; new clients tolerate missing or unknown fields. `cosmon-remote`
prints the reason, one locally mapped next action and the request ID; command
hints are never executable strings supplied by the server.

## 12. Surface additions (implemented by W6–W7)

**Routes** (each registered in `docs/guides/api-cli-coverage.md`,
`openapi/v1.yaml`, `tests/api_surface_freeze.rs` and the surface canon, per
§8p):

| Route | Authorization |
|---|---|
| `PUT /v1/admin/noyaux/{noyau}/harvest-authority` | admin credential; compare-and-set of policy, public root and epoch; replacing an installed root also requires `rotation_signature` from the current key over the tenant, old and new root digests, and next epoch; no private-key field |
| `GET /v1/harvest/status` | tenant admission plus `read` or harvest scope (legacy `write` implies `read`) |
| `POST /v1/harvest/challenge` | tenant admission plus harvest scope, or legacy R2 `write`; returns facts and canonical bytes, creates no authority |
| `POST /v1/harvest/grants` | same as challenge; verifies before atomic installation; does not consume |

The admin route follows the existing admin-surface classification
(`principal=operator`, adapter-only, no tenant scope). The tenant routes'
local counterparts are `cs harvest-authority configure|status|challenge|import`,
delegating to the same library. `cs harvest` is **not** reused: it is the
deprecated alias for `done --if-completed` and has callers.

**Operator commands** (`cosmon-remote`, on the operator device):

| Command | Effect |
|---|---|
| `harvest configure --policy scoped\|sealed\|disabled --admin-token-file <f>` | selects the policy through the admin route (compare-and-set) |
| `harvest init --admin-token-file <f>` | generates a fresh key through the external signer, installs only the public half, selects `sealed`; idempotent for an identical setup; rotation names the old fingerprint and signs the canonical rotation statement with the current key |
| `harvest grant --molecule <id> \| --mission <id> [--expires-in 1h \| --no-expiry]` | fetches the challenge, rebuilds the canonical bytes locally and compares, shows them, signs, uploads; reports an installation receipt, never a merge |
| `harvest grant --export <file> \| --sign <file> \| --import <file>` | offline issuance; the three modes are mutually exclusive |
| `harvest status [--molecule <id>]` | effective policy and provenance (JSON fields `policy` and `provenance`, values as in §2), required scopes, executor support, key fingerprint, epoch, grant validity; writes nothing |

## 13. Decision OQ-1 — the harvest scope's grant source

The operator selected **binding-only** on 2026-09-29. The dedicated harvest
scope is honoured only when the administrator's binding for the exact
issuer/subject/audience grants it. A bearer token carrying that scope does
not grant harvest authority when the binding omits it. Other scopes retain
their existing token-or-binding source rule; changing that general rule is a
separate follow-up. This check applies again at the effect boundary so a
binding removed after route admission cannot authorize an effect.

## 14. Acceptance cases

`tests/e2e/test_harvest_profiles.py` carries cases per contract clause. Its
client-only operator surface cases run by default. The remaining container
cases are marked `contract_pending` and **deselected unless
`RPP_E2E_CONTRACT_PENDING=1`**. Deselection, not skip: a case that has not run
never reports green. The nightly container job does not set the variable.
W9 owns the remaining prerequisites and their final container verdicts.

| Case | Clause |
|---|---|
| `TestOperatorSurface::*` (client only, no stack) | §12 commands exist; R8 closed policy values; offline modes exclusive |
| `TestScopedConflict::test_configure_scoped_refuses_and_writes_nothing` | R5, §4 |
| `TestScopedProfile::test_sensitive_option_is_refused` | §5 |
| `TestScopedProfile::test_harvest_scope_merges_without_key_or_grant` | R4 |
| `TestScopedProfile::test_status_reports_explicit_scoped` | §2, §12 status |
| `TestScopedWithoutHarvestScope::test_write_scope_is_not_sufficient` | R4, §11 |
| `TestSealedProfile::test_key_stays_on_the_operator_side` | R6/R7, §8 |
| `TestSealedProfile::test_missing_grant_is_typed` | R6/R7, §11 |
| `TestSealedProfile::test_production_grant_then_done_merges` | R6/R7, §8, §12 |
| `TestLegacyUpgrade::test_absent_policy_stays_disabled` | R1, §4 |
| `TestDisabledProfile::test_valid_grant_does_not_enable` | R3 |

R2 (legacy sealed, `write` plus a grant) is already asserted by
`test_tenant_journey.py`, which must stay green unchanged through #120. R8
beyond the closed enum (malformed or unreadable configuration) is asserted
in-process by the unit that implements the resolver, where the fault can be
injected deterministically. A case that needs a credential carrying *only*
the harvest scope is owed by the final container acceptance unit (W9), whose
harness can issue a second identity.

Some cases need what the stock harness does not provision yet: the stack's
admin credential, a hook that issues the harvest scope through the binding,
and an external signer. Their fixtures fail as *prerequisite missing*,
distinct from a contract verdict.

Baseline observations for these cases are recorded in the W0 molecule
report, not here.

## 15. Out of scope

Stronger worker or service containment — separate repository or ref custody,
a separate principal — is outside #120. The guarantee stays "authorized
cosmon harvest" (ADR-172 D5): none of this prevents a same-uid process from
calling Git directly.
