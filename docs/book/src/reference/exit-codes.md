# Exit codes & JSON output

> These commands use physics-inspired names (nucleate, evolve, decay, spore, …). New to the vocabulary? See [The physics vocabulary](../explanation/physics-vocabulary.md).

Every `cs` command is scriptable: it returns a typed exit code and, with
`--json`, machine-readable output. This page is the contract a worker or
external scheduler branches on.

> This page is **hand-written** (it documents runtime behaviour, not a
> command signature) and is covered by the command-name grep + link check,
> not the generated golden diff. See the [CLI overview](./overview.md) for
> the generated command pages.

## The `--json` convention

`--json` is accepted on every command (an agent-first interface). Human
output goes to `stdout` as a rendered view; `--json` replaces it with
JSON: one object, or NDJSON (one object per line) for list-shaped
commands. Errors under `--json` are emitted to `stderr` as
`{"error": "<message>"}`; the exit code still carries the typed reason.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `1` | Generic failure (an unclassified error; the message is on `stderr`). |
| `2` | A session is already open (`cs journal start` when one is live). |
| `3` | No open session (`cs journal note`/`end` with nothing to write to). |
| `10` | Guard refusal: missing parent link (a decay/merge child lacks its typed edge back to the parent). |
| `11` | Guard refusal: a decay produced a homogeneous count that the type-tightening guard rejects. |
| `12` | Guard refusal: dirty-backlog runtime refusal (a greedy runtime would resurrect stale pendings; ADR-048). |
| `13` | Guard refusal: broker-spawn refusal (a self-referential spawn the Gödel guard forbids). |
| `14` | Guard refusal: decomposition depth-limit exceeded (the Gödel depth guard). |
| `15` | Guard refusal: governance tier does not descend (ordinal stratification: a child may not out-rank its parent). |
| `16` | Guard refusal: briefless dispatch (`cs tackle` on a molecule whose formula's required, default-free variables are missing or blank — a worker would spawn with no Mission). |
| `17` | Guard refusal: the formula requires worker capabilities the resolved adapter lacks (`requires_capabilities = ["shell", …]` on a chat-only local adapter). Re-run with a coding-agent `--adapter`, or set `COSMON_SKIP_CAPABILITY_GATE=1`. |
| `70` | Harvest refusal `not_completed`: `cs done` on a molecule that is not `Completed`. |
| `71` | Harvest refusal `not_authorized`: the galaxy armed `[harvest_authority]` and no operator grant covers this molecule. |
| `72` | Harvest refusal `reservation_requires_seal`: the molecule carries a reservation only a human can lift (`hold:human`, `needs-review`, `security`, `no-auto-harvest`, `harvest_to:*`). |
| `73` | Harvest refusal `backlog_full`: the sealed threshold of closed-but-not-integrated molecules is reached. |
| `74` | Harvest refusal `merge_conflict`: nothing was merged or torn down; the branch and worktree stand. |
| `75` | Harvest refusal `base_not_fast_forward`: the base cannot accept the configured merge policy (operator configuration). |
| `76` | Harvest refusal `pre_done_refused`: the blocking `[hooks] pre_done` gate refused. |
| `77` | Harvest refusal `missing_reason`: the request carried no reason (the Remote Pilot Port requires one; `cs done --reason` is optional). |
| `78` | Harvest refusal `protected_path_modified`: the branch changed a path declared with `cs nucleate --protect`. `cs done --allow-protected-change` overrides it at the terminal. |
| `90` | `cs run` stopped: action budget (`--max-actions`) exhausted. |
| `91` | `cs run` refused or stopped: more than `--max-molecules` molecules in the fleet. |
| `92` | `cs run` refused the plan: deeper than `--max-depth` (nothing was started). |
| `93` | `cs run` finished with a failed teardown: a molecule's branch is not integrated and its worktree still stands. |
| `94` | `cs run` stopped on a permanent dispatch refusal (for example an unsupported step kind). |
| `124` | `cs run --timeout` reached its deadline. |

Codes `70` to `78` are the nine **harvest door refusals** of `cs done`, with the
same labels on the wire (`POST /v1/molecules/{id}/done`). Codes `90` to `94`
and `124` are the named stops of `cs run`.

Codes `10` to `17` are the **typed CLI guard refusals**: a script can branch
on the specific invariant that fired rather than treating every non-zero
exit as the same failure. Codes `2` to `3` are the session-carnet guards.
Any other error falls through to the generic `1`.

`16` and `17` are additionally the **permanent** refusals: unlike the
others, an identical retry reproduces them exactly, so the resident runtime
(`cs run`) parks such a molecule rather than re-dispatching it every tick.
A non-zero `permanently_parked` in the run summary counts them.

## Example

```console
$ cs decay <mol> --into 1        # homogeneous count → guard refusal
cs: decay would produce a homogeneous 1-child result …
$ echo $?
11
```
