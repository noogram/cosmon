<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Port or wire: deciding what an HTTP test is the proof of

A test that binds a TCP socket and one that calls the router in-process
look almost identical in the diff. They are not the same test, and the
difference is not speed. This note gives the rule for choosing, the
obligation that comes with converting, and the classification it
produced for `cosmon-api` on 2026-08-06.

It is written for any HTTP surface in this workspace, not only
`cs-api`. The same question applies to `cosmon-rpp-adapter` and to any
adapter added later.

## The question

For each test, one question, asked before anything else:

> **What is this test the proof of?**

Not "what does it exercise" — everything exercises everything. What
would a reader be entitled to conclude from it being green, and what
would they be entitled to conclude from it being red.

Two answers, and only two:

**The claim is about our logic.** Which route was selected, what got
serialised, which request was refused and why, what the handler
projected out of on-disk state, what side effect it did or did not
perform. For these, the socket proves nothing extra. The bytes that
travel over loopback are produced by hyper, which we did not write and
are not testing. The socket costs a bind, a port, a scheduling
handoff, and — this is the part that bites — a dependency on the
environment the test happens to run in. Its place is behind an
injectable port, which for an axum service is the `Router` itself,
driven by `tower::ServiceExt::oneshot`. This is the hexagonal doctrine
the repository already states for the domain core, applied one layer
out.

**The claim is about the wire.** Does `axum::serve` on a bound listener
actually answer. Does a request that never enters our code still get a
well-formed response. Does the process that `main` composes listen
where it said it would. For these, replacing the socket with a
double means testing the double. Keep the socket.

The boundary is sharper than it first looks, and it moves as code
moves. Cross-origin refusal *sounds* like a wire property. In
`cs-api` it is not: `cosmon_api::cors::layer` is a hand-written
middleware in this repository, and the property that matters — the
refusal is returned *before* `next.run` is awaited, so the handler
never spawns anything — is a property of that function's control flow.
A socket does not observe it any better than a `oneshot` does. Had we
mounted `tower_http::cors::CorsLayer` instead, the same test would be
proving somebody else's code and the answer would flip.

So the rule is not "integration tests use sockets". It is: **the
socket is justified exactly when the code under test is the socket.**

## The obligation when converting

A test moved behind a port must keep the ability to go red. This is
not a formality. Four tests in this workspace in the week of
2026-08-06 were found to be testing nothing while passing:

- an M8 falsifier measuring a mission that had no lease to measure;
- a `wait_ready` in the `@jdthaler` harness that never observed a ready
  composer, and would have waited the same way for one that never came;
- an assert-guard that failed *closed* on a shell injection, so the
  injection case and the correct case were indistinguishable;
- a guard test that passed because the lease it guarded was invisible
  to it.

Every one of them was green. Green is not evidence.

So each conversion carries a **red proof**: remove the behaviour the
test claims to prove, run the test, watch it fail, restore. Record the
observed failure in the commit or in the test's own comment. A
conversion without a red proof is a conversion that may have deleted
the assertion along with the socket.

The red proof also catches the specific way a `oneshot` conversion goes
wrong. Over a socket, a handler that panics surfaces as a connection
error or a 500. In-process, `oneshot` returns `Result` and a `.unwrap()`
on it hides nothing — but a helper that swallows a non-2xx status, or
an assertion written against `resp.status()` when the harness already
asserted success, quietly loses teeth. Removing the behaviour is what
tells you.

## What speed is and is not

Measured on `cosmon-api` before any change, on 2026-08-06, with the
test binaries run directly and serially:

| suite | tests | wall clock, parallel |
|---|---:|---:|
| `tests/smoke.rs` | 24 | 0.16 s |
| `tests/cors.rs` | 6 | 0.84 s |

The whole thing was already about a second. **Speed was never the
argument here, and this note exists partly to say so out loud.** The
mission that produced it explicitly warned against converting a wire
test to save time; the measurement says there was no time to save.

After the conversion, same method, five runs each:

| suite | tests | wall clock, parallel |
|---|---:|---:|
| `tests/smoke.rs` | 24 | 0.17 – 0.23 s |
| `tests/cors.rs` | 6 | 0.70 – 0.82 s |
| `tests/wire.rs` (new) | 5 | 0.08 – 0.16 s |

About 1.0 s before, about 1.05 s after — the new suite is a third test
binary and costs a process start. **The conversion made the suite very
slightly slower.** That is the honest number, and it is the right
trade: what changed is not the clock but the count of loopback binds,
from roughly 28 to 2.

What the socket actually costs on this suite is not seconds, it is
*determinacy*. A loopback bind is a shared, ambient resource. It
depends on the sandbox the process runs in, on proxy and egress
variables in the worker's environment, and on there being a free
ephemeral port. That is why molecule `task-20260806-4823` exists at
all: worker environment poisoning these same tests. Converting the
logic tests does not fix that — it shrinks the surface it can reach
from thirty tests to one.

If you find yourself reaching for this note to make a suite faster,
you are using it for the wrong thing. Use it to make a suite *say
something true*.

## Classification — `cosmon-api`, 2026-08-06

`tests/cors.rs`, 6 tests:

| test | verdict | why |
|---|---|---|
| `a_cross_origin_simple_post_never_reaches_the_worker_spawn` | logic | The claim is an ordering inside `cors::layer`: refusal is returned before `next.run`, so no subprocess is spawned. Observed on a marker file our own stand-in `cs` writes. Nothing in the claim mentions a socket. |
| `under_deny_a_cross_origin_simple_post_never_reaches_the_worker_spawn` | logic | Same claim under the default policy. Same reasoning. |
| `default_router_emits_no_allow_origin` | logic | Asserts our middleware injects no `Access-Control-Allow-Origin`. The header map is ours either way. |
| `denied_origin_gets_no_allow_header_on_a_write_route` | logic | Status and header both produced by `cors::layer`. |
| `preflight_from_an_unlisted_origin_is_refused` | logic | The `OPTIONS` branch is a hand-written arm of `cors::layer`, not axum's. |
| `listed_origin_is_echoed_exactly_and_varies` | logic | Exact echo and `Vary` are lines in `cors::inject`. |

`tests/smoke.rs`, 21 HTTP tests (plus 3 `prebuilt::` unit tests that
never touched HTTP):

| test | verdict | why |
|---|---|---|
| `healthz_returns_ok_true_and_cs_version` | logic | Claim is about the JSON the handler builds and about a `cs --version` subprocess. The subprocess is a real port and stays real; the socket is incidental. |
| `start_twice_returns_409` | logic | Claim: a second open on an open carnet is a conflict. Session-state logic. |
| `note_without_session_returns_409` | logic | Same shape: a precondition our handler checks. |
| `end_seals_session_with_notes` | logic | Claim is about the seal format and the note count our code computes. |
| `current_reports_open_session_notes` | logic | A projection of in-memory session state into JSON. |
| `e2e_session_survives_to_disk` | logic | Claim is about bytes our code wrote to a tempdir. The filesystem is the port under test; the socket is not. |
| `whispers_lists_files_newest_first` | logic | Ordering and frontmatter parsing, both ours. |
| `whispers_respects_limit_and_empty_inbox` | logic | Query-parameter handling and a missing-directory branch. |
| `whisper_archive_moves_file_to_archived_tree` | logic | Claim is a file move plus a 404 branch. |
| `whisper_spark_nucleates_molecule` | logic | Claim is a molecule landing on disk under the scoped state dir. |
| `inbox_filters_by_status` | logic | Default filter and `status=all`, both our query logic. |
| `galaxies_scans_dir_and_counts_pending` | logic | A directory scan and its counters. |
| `ensemble_aggregates_workers_and_molecules_per_galaxy` | logic | Aggregation arithmetic over on-disk state. |
| `ensemble_allowlist_and_status_filter` | logic | Allow-list and status filtering in the query layer. |
| `peek_returns_monospace_text_at_city_scale` | logic | Text rendering. |
| `peek_building_default_and_skin_focus` | logic | Scale defaulting and focus selection. |
| `tag_molecule_adds_and_removes_tags` | logic | A state mutation, re-read through another route. |
| `tag_molecule_rejects_empty_payload` | logic | An input-validation branch. |
| `tag_molecule_rejects_dangerous_ids_and_tags` | logic | Two input-validation branches, one on the path segment and one on the body. |
| `instrumentation_emits_one_event_per_call_with_correct_mode` | logic | Claim is about NDJSON our instrumentation writes and the `InvocationMode` it records. |
| `observe_molecule_emits_in_process_state_read_event` | logic | Same, plus the authz event. The whole point is that the route does *not* shell out — a claim about our call graph. |

**Twenty-seven of twenty-seven HTTP tests are logic tests.** That is a
larger number than it feels like it should be, and it is worth stating
plainly why: `cs-api`'s tests never asserted anything about the wire
in the first place. They used a socket because `reqwest` was the
convenient way to write an assertion, not because the socket carried a
claim. Thirty tests were each paying for a bind in order to prove
things about JSON, files, and control flow.

### The one wire test that did not exist

The honest consequence of the table above is that the wire had *no*
test — its coverage was smeared across thirty tests that were each
about something else, which means a break in `axum::serve` composition
would have failed thirty tests and none of them would have named the
cause.

So the conversion adds one, `tests/wire.rs`, which is deliberately the
only place in this crate that binds a listener. It asserts what only a
socket can: that the router composed the way `main` composes it, served
by `axum::serve` on a real bound port, answers a request and refuses a
cross-origin write. When it is red, the thing that is broken is the
serving path. When it is green, every other suite is entitled to skip
the socket.

That is the shape to aim for on any adapter: **one wire test, named as
such, and everything else behind the port.**

### `tests/loadtest.rs`

Left on the socket, and it is not an exception to the rule. It is
`#[ignore]`d, runs against live state on request, and its output is a
latency table copied into a mini-report. Latency measured through a
real listener is the number it is reporting; measuring it through a
`oneshot` would report a different quantity under the same name.

## The same question, asked of `cosmon-rpp-adapter`

Asked, and answered by the code: **that crate has already done this.**
Of its 32 test suites, measured 2026-08-06:

- 22 drive `router(...)` through `oneshot` — no listener at all;
- 10 bind nothing and speak to no router (unit tests, proptests,
  file-shape and env-hygiene checks);
- 1 binds a listener, in `tests/auth_claude_integration.rs`, and it is
  the right call: the bind is a **mock upstream Anthropic server**, and
  the claim is that the adapter's outbound HTTP client really talks to
  a real endpoint. That is a wire claim about a wire we drive. Nothing
  to convert.

So the pattern the question was asked about — a socket carrying a claim
that never needed one — was specific to `cs-api`. `cosmon-rpp-adapter`
is the model, not the backlog. Any suite added there should be read
against the table above before it grows a listener.

## What the red proofs actually caught

The obligation is not decorative. Producing a red proof for all 27
conversions took one universal mutation plus twenty targeted ones, and
it turned up two tests that were passing for the wrong reason —
**before** the conversion, not because of it. Both were in
`tests/smoke.rs`:

- `tag_molecule_rejects_empty_payload` posted `{"add":[],"remove":[]}`
  to `/molecules/task-tag-empty/tag` and asserted `400`. It got one, but
  from `MoleculeId::new` — `tag` is not a `YYYYMMDD` date, so the id was
  rejected before the empty-payload branch was ever reached. Deleting
  the guard the test is named after left it green.
- `tag_molecule_rejects_dangerous_ids_and_tags` did the same thing in
  its second half: `{"add":["--force"]}` on `/molecules/task-safe/tag`,
  where `task-safe` is likewise not a legal id. The tag guard was never
  on the path.

Both now use well-formed ids and assert on the error message, so the id
refusal and the payload refusal cannot be mistaken for one another
again. Neither would have been found by reading the tests; both fell
out of asking "what happens if I remove the thing this proves?"

The same exercise also documented something worth keeping: several of
these guards are **defence in depth**, and a single-layer mutation
leaves the test green *correctly*. `validate_molecule_id` is backed by
`MoleculeId::new`; the handler's empty-payload check is backed by
`TagError::EmptyRequest`; `validate_tag`'s leading-dash rule is backed
by `Tag::new`. A red proof that has to remove two layers is not a weak
red proof — it is the map of a guard that was built twice on purpose.
Record which layer you removed.

## Checklist

Before you write a new HTTP test:

- [ ] Say in one sentence what the test is the proof of.
- [ ] If that sentence names our routing, serialisation, refusal,
      projection, or side effect → `Router` + `oneshot`.
- [ ] If it names the listener, the serve loop, or bytes on a socket →
      bind, and put it in the crate's single wire suite.
- [ ] If you cannot write the sentence, the test is not ready.

Before you convert an existing one:

- [ ] Classify it with the sentence.
- [ ] Convert.
- [ ] Break the behaviour it claims to prove; observe red; restore.
- [ ] Record the red proof.
