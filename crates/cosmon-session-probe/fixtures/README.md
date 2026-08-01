# Session-probe fixtures

Synthetic provider logs, shaped exactly like the real ones and containing
nothing that came from a real one.

The mission's M1 acceptance clause opens with **"fixtures without secrets"**.
That is not satisfied by redacting a captured log — a redaction is a claim
about what you removed, and the next reader cannot check it. These files were
written from the *record-type histograms and key unions* in the ADR-168 traces
instead, so there is nothing to have missed:

- every identifier is a deliberately non-random placeholder
  (`00000000-0000-4000-8000-…`, `fixture-…`);
- every path is under `/fixture/`, a directory that exists on no host;
- every message body is the literal text `<fixture>`, and the normalised
  events carry only its *length* anyway (see `event.rs`);
- no token, key, cookie, email address, hostname or real branch name appears.

`tests/acceptance_m1.rs` asserts the last point mechanically — a test scans
these files for credential shapes, `@` addresses and `$HOME`-derived paths, so
a future fixture pasted in from a real session fails the suite rather than the
review.

## Layout

```text
claude/projects/-fixture-decoy-galaxy/
  00000000-0000-4000-8000-000000000001.jsonl   # no title — an unnamed session
  00000000-0000-4000-8000-000000000002.jsonl   # an `ai-title` record — a named one
codex/sessions/2026/08/01/
  rollout-2026-08-01T00-00-00-…-0000000000c1.jsonl
  rollout-2026-08-01T02-00-00-…-0000000000c2.jsonl
```

All four announce the same working directory, `/fixture/galaxy` — that is what
makes "two sessions in one cwd" testable per provider, and one named plus one
unnamed Claude session is what makes "a name is an alias, not a key" testable.

The Claude tree keeps the `projects/<sanitised-cwd>/` layer because the
adapter's contract is that it *ignores* that directory name (probe P6). The
directory is deliberately named `-fixture-decoy-galaxy`, which decodes to
`/fixture/decoy/galaxy` — a path no fixture log uses. An adapter that read the
directory name instead of the record would resolve the wrong working directory
and `clause_the_project_directory_name_is_never_decoded` would fail.
