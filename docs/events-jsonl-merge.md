# Auto-merging append-only JSONL during `cs done`

## The problem

`.cosmon/state/events.jsonl` and `.cosmon/state/interactions.jsonl` are
tracked append-only logs. Both `main` and any feature branch append new
entries during the life of the branch, so when `cs done` merges the branch
back into main, git's 3-way merge sees adjacent hunks on both sides and
reports a textual conflict — even though there is no *semantic* conflict:
each line is an independent, self-contained JSON record.

Before this fix, `cs done` aborted the merge and asked the operator to
resolve the conflict by hand. In practice the "resolution" was always the
same: accept both sides, sort by timestamp, move on.

## The fix

When `try_merge_branch` detects a conflict and every unmerged file is an
append-only JSONL file (see `APPEND_ONLY_JSONL_BASENAMES` in
`crates/cosmon-cli/src/cmd/done.rs`), the resolver:

1. Reads both sides of each conflicting file from the git index
   (`git show :2:path` for *ours*, `git show :3:path` for *theirs*).
2. Takes the set-union of their non-empty lines.
3. Sorts the union by the `"timestamp"` field (lexicographic RFC3339 order,
   which matches chronological order for UTC timestamps).
4. Writes the merged blob, `git add`s each file, and finalizes with
   `git commit --no-edit`.

If the union/commit fails for any reason, the resolver returns an error
and falls through to the original abort path — so the operator still has
the manual-resolution escape hatch.

## Scope and safety

- Only files whose basename is `events.jsonl` or `interactions.jsonl` are
  eligible. `fleet.json`, molecule `state.json`, or any other JSON file
  still aborts on conflict.
- The set-union dedupes lines that are byte-identical on both sides, which
  handles the common "both branches copied the same seed entry" case.
- Timestamp extraction is a small string scan for `"timestamp":"..."`; if
  the field is missing, the line is sorted to the front (empty key). Lines
  without timestamps are not expected in the schema; the fallback keeps the
  resolver robust.

## Test coverage

See `crates/cosmon-cli/src/cmd/done.rs`:

- `test_merge_jsonl_by_timestamp_unions_and_sorts` — pure-function union + sort.
- `test_is_append_only_jsonl_recognizes_events_and_interactions` — classifier.
- `test_merge_branch_auto_resolves_events_jsonl_conflict` — end-to-end git
  repro: both sides append, `try_merge_branch` returns `Merged`, no
  `MERGE_HEAD` left behind, final file has both entries in timestamp order.
