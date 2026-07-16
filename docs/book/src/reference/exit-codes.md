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
| `2` | A session is already open (`cs session start` when one is live). |
| `3` | No open session (`cs session note`/`end` with nothing to write to). |
| `10` | Guard refusal: missing parent link (a decay/merge child lacks its typed edge back to the parent). |
| `11` | Guard refusal: a decay produced a homogeneous count that the type-tightening guard rejects. |
| `12` | Guard refusal: dirty-backlog runtime refusal (a greedy runtime would resurrect stale pendings; ADR-048). |
| `13` | Guard refusal: broker-spawn refusal (a self-referential spawn the Gödel guard forbids). |
| `14` | Guard refusal: decomposition depth-limit exceeded (the Gödel depth guard). |
| `15` | Guard refusal: governance tier does not descend (ordinal stratification: a child may not out-rank its parent). |

Codes `10` to `15` are the **typed CLI guard refusals**: a script can branch
on the specific invariant that fired rather than treating every non-zero
exit as the same failure. Codes `2` to `3` are the session-carnet guards.
Any other error falls through to the generic `1`.

## Example

```console
$ cs decay <mol> --into 1        # homogeneous count → guard refusal
cs: decay would produce a homogeneous 1-child result …
$ echo $?
11
```
