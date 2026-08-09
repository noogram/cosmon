<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Cockpit surface canon v1

The cockpit UI is a separate project, so its boundary with cosmon is a
versioned data contract rather than a shared source tree. Cosmon owns the
declarations and gates them here; an external UI consumes the committed JSON
projection at a pinned cosmon revision and gates its own implementation against
the same view identifiers.

This contract implements ADR-171's per-view boundary. A cockpit is a command
surface, but that does not exempt all of its views from the wheat-paste rule:
a view with a canonical `cs` raster is a `viewport`; only a view without one is
`command`. If `cs` later gains a canonical raster for a command view, the next
view declaration must classify its replacement as `viewport`. The boundary
moves only in that direction.

## Why this is a second log

The cockpit canon is not an extension of
`crates/cosmon-rpp-adapter/data/surface_events.txt`. That log declares an HTTP
surface for remote JWT principals, OAuth scopes, and §8p exposure. The cockpit
canon declares local-human views and their revealable `cs` sources. Combining
the two would either put meaningless OAuth fields on local views or weaken the
RPP's security vocabulary.

Both logs use the same append-only precedent and the same parser crate, but
their records and compatibility rules remain distinct.

## Canonical source format

The source is
`crates/cosmon-cockpit/data/cockpit_views.txt`. Blank lines and lines beginning
with `#` are ignored. Every other line is one immutable `view_added` event with
eight trimmed, pipe-separated fields:

```text
view | source_cs | stability | introduced | class | molecule_id | date | blurb
```

| Field | Contract |
|---|---|
| `view` | Stable lower-kebab-case identifier. It is introduced once and never reused. |
| `source_cs` | Exact revealable `cs <verb> ...` invocation that supplies the state. |
| `stability` | `experimental` while the workflow is measured, or `stable` once compatibility is promised. |
| `introduced` | First shipping cosmon version as `MAJOR.MINOR.PATCH`, without `v`. |
| `class` | `viewport` when a canonical raster exists; otherwise `command`, under ADR-171 §8ac. |
| `molecule_id` | Molecule or ADR that introduced the declaration. |
| `date` | Introduction date as `YYYY-MM-DD`. |
| `blurb` | Non-empty, one-line explanation for human consumers. |

The separator `|` is forbidden inside a field. Duplicate view identifiers,
unknown enum tokens, malformed versions or dates, and source commands not
beginning with `cs <verb>` are errors. Existing event lines are never edited,
removed, or reordered. An incompatible replacement receives a new `view`
identifier; an experimental declaration is not permission to mutate history.

## Consumer artifact

External projects consume
`crates/cosmon-cockpit/data/cockpit_views.v1.json`, preferably via a raw file
at a pinned commit or release tag. It is a deterministic projection, not a
second authority:

```json
{
  "schema_version": 1,
  "views": [
    {
      "view": "fleet-overview",
      "source_cs": "cs peek --json",
      "stability": "experimental",
      "introduced": "0.5.0",
      "surface_class": "viewport",
      "molecule_id": "task-20260804-2bbb",
      "date": "2026-08-06",
      "blurb": "Fleet and molecule lifecycle overview"
    }
  ]
}
```

Consumers must reject an unknown `schema_version`. They may implement only a
subset of declared views, but must not implement an undeclared view or invent
domain vocabulary. For a `command` view, ADR-171 §8ac additionally requires UI
tokens to be a subset of the tokens emitted by cosmon and requires attribution
to the source command and byte range.

## Gates on the cosmon side

Two complementary checks make the boundary fail closed:

1. `cosmon-cockpit/build.rs` parses the append-only source through
   `cosmon-surface-canon`, renders schema v1 deterministically, and byte-compares
   it with the committed JSON artifact. A malformed declaration or stale
   consumer artifact fails every workspace build.
2. `cosmon-cli/tests/cockpit_surface_canon.rs` derives command paths from every
   `source_cs` field and compares them with the live clap tree exposed by
   `cs __help-tree --all`. Removing or renaming a declared source command fails
   the test suite.

The external peer owns the other half-gate: its CI loads the pinned JSON,
asserts that every implemented view identifier is declared, and publishes its
coverage attestation as required by ADR-171's amendment to §8l. Cosmon cannot
truthfully run that repository's tests; the versioned JSON is the named seam
between the two gates.

## Minimal declarations

The initial log deliberately contains two views that exercise both classes:

- `fleet-overview` is a `viewport` sourced from `cs peek --json`; the canonical
  `cs peek` raster exists, so §8k′ applies.
- `provider-sessions` is a `command` view sourced from
  `cs sessions discover --json`; no canonical raster exists, so ADR-171 §8ac
  applies. Its name also avoids presenting the ambiguous bare label
  “Sessions”; the future `cs pilots` rename can introduce a successor event
  without rewriting this history.
