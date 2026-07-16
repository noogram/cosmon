# Whisper frontmatter schema

Canonical reference for the YAML frontmatter block at the head of
every whisper markdown file on disk. One schema covers every origin —
typed text, voice-only (pre-STT), inline-dictated voice, post-hoc
transcribed voice, and sibling-file edits. Consumers parse a single
shape and branch on two keys: `source` and `authored_via`.

**Scope.** Defines the schema and the read-side parsing contract for
Tier 1, Tier 2, Tier 3, and edit siblings. The inline admission writer
(crate `cosmon-matrix-tick`) emits Tier 1 and Tier 2 today; Tier 3 is
produced by the STT enrichment pass (`task-20260422-ecfb`); the edit
sibling is wired by the editor UI (separate molecule — not yet
started). This document is the binding target for all three.

**Provenance.** Seven-persona deliberation `delib-20260422-6e2c`
(voice messages). JR fixed the key name (`authored_via`). Einstein
set the §8m closure (per-segment confidence). Torvalds set the v0
shippable tier (`untranscribed`). Shannon set the irreducible field
set.

## Two orthogonal axes

Every whisper carries two independent classifiers in its frontmatter:

| Key | Axis | Values | Meaning |
|---|---|---|---|
| `source` | **ingress substrate** | `matrix`, `capture`, `edit`, … | Which channel the event entered from. |
| `authored_via` | **authorship modality** | `typed`, `untranscribed`, `dictated`, `transcribed`, `edited` | How the body came to be. |

These are independent. A voice note dictated through a Matrix client
reports `source: matrix` + `authored_via: dictated`. A typed message
reports `source: matrix` + `authored_via: typed`. A post-hoc
hand-edit reports `source: edit` + `authored_via: edited`.

### Why `authored_via` and not overload `source`?

Overloading `source` to carry authorship modality conflates two
questions a reader must answer separately: "where did this whisper
come from?" (ingress channel governance, rate limits, trust) and
"how do I read the body?" (placeholder vs. transcript vs. typed
text). JR's verdict: name them separately.

### Migration — zero-cost

A pre-schema whisper (no `authored_via` key on disk) parses as
`authored_via: typed`. The consuming struct
(`WhisperFrontmatter::authored_via`) carries `#[serde(default)]`,
and the enum's `#[derive(Default)]` resolves to `Typed`. No
retrofit of existing files is required.

## Three density tiers

The same frontmatter shape covers three tiers of provenance density.
A parser inspects `authored_via` + the presence of optional keys to
decide which tier applies.

### Tier 1 — `authored_via: typed`

Status quo. Human typed the body. No audio archive.

```yaml
---
event_id: "$abc..."
sender_mxid: "@tenant_auditor:example.org"
sender_nucleon_id: "tenant_auditor"
origin_server_ts: 1745373600000
room_id: "!whispers:example.org"
msgtype: "m.text"
source: matrix
authored_via: "typed"
received_at: "2026-04-23T02:17:42Z"
---

hello world
```

The `authored_via` key is emitted by the current writer even for
Tier 1 — self-describing beats "absence ≡ default" archaeology.
Legacy files without the key still parse correctly.

### Tier 2 — `authored_via: untranscribed`

Voice whisper where the audio blob has been archived but STT has not
run yet. Torvalds v0 — full provenance without transcription noise.

```yaml
---
event_id: "$abc..."
sender_mxid: "@tenant_auditor:example.org"
sender_nucleon_id: "tenant_auditor"
origin_server_ts: 1745373600000
room_id: "!whispers:example.org"
msgtype: "m.audio"
source: matrix
authored_via: "untranscribed"
mxc_url: "mxc://example.org/xyz..."
audio_sha256: "deadbeef..."
audio_bytes_path: ".cosmon/whispers/audio/deadbeef....ogg"
duration_sec: 32
received_at: "2026-04-23T02:17:42Z"
---

[audio · 32s · mxc://example.org/xyz...]
```

**Body rule.** JR's monospace placeholder:
`[audio · <N>s · <mxc-uri>]`. One line, no blockquote, no chrome.
QuickLook-playable through the `mxc_url`.

**mxc_url + audio_bytes_path — belt and suspenders.** Einstein's
belt-and-suspenders rule: `audio_bytes_path` may get garbage-collected
on disk (unlikely); the Matrix homeserver may GC the `mxc_url`
(Synapse defaults to 90 days). Carrying both keeps the recovery
surface maximal.

### Tier 3 — `authored_via: dictated` or `transcribed`

Tier 2 + STT provenance. Every transcribed word is auditable: the
model identity, the timestamp of the transcription pass, and
per-segment confidence are on disk with the body.

```yaml
---
event_id: "$abc..."
sender_mxid: "@tenant_auditor:example.org"
sender_nucleon_id: "tenant_auditor"
origin_server_ts: 1745373600000
room_id: "!whispers:example.org"
msgtype: "m.audio"
source: matrix
authored_via: "dictated"
mxc_url: "mxc://example.org/xyz..."
audio_sha256: "deadbeef..."
audio_bytes_path: ".cosmon/whispers/audio/deadbeef....ogg"
duration_sec: 32
transcription_model: "openai-whisper@whisper-1"
transcription_timestamp: "2026-04-23T02:18:04Z"
transcription_segments:
  - { start: 0.0, end: 2.4, text: "Hello world", avg_logprob: -0.18 }
  - { start: 2.4, end: 4.0, text: "ship it", avg_logprob: -0.42 }
received_at: "2026-04-23T02:17:42Z"
---

Hello world ship it
```

**Dictated vs. transcribed.** Both are Tier 3 with identical shape.
Distinction:

- `dictated` — voice + inline ASR at ingest time. The author intended
  the text; the transcript is the canonical body.
- `transcribed` — archival voice (originally admitted as Tier 2
  `untranscribed`) that a later post-hoc STT pass enriched. The body
  is a best-effort reconstruction — the reader should weight the
  per-segment `avg_logprob` accordingly.

**Body rule.** The transcript replaces the placeholder. The reader
may visually mark low-confidence spans using `transcription_segments`.

**§8m closure.** Einstein's rule: confidence is per-segment, not a
scalar mean. A 30-second voice note with 28 seconds of clear speech
and 2 seconds of mumbling is *not* "68% confident" — it is "clear
here, unclear there." The `transcription_segments` array carries
the per-span `avg_logprob` so the reader can surface the local
truth. A scalar `transcription_confidence` key is explicitly
**not** part of this schema.

### Empty-segments case

If a reader detects `authored_via in {dictated, transcribed}` but
`transcription_segments` is absent, it is the low-confidence
fallback case (the model failed to produce segment-level data).
For uniformity the writer emits `transcription_segments: []`
instead — the key is always present when Tier 3 applies, so the
reader never has to distinguish "missing" from "empty."

## Edits — sibling files, never in-place

Einstein's rule: edits produce a **sibling file** referencing the
original `event_id`. The raw transcript is never mutated in place.
Edit files live next to the original with the suffix
`-<edit_stamp>-edit.md` (exact naming convention left to the editor
implementation — `task-tbd`).

```yaml
---
source: edit
authored_via: "edited"
event_id: "$abc..."
edit_of: ".cosmon/whispers/inbox/!room/1234-_abc.md"
edited_by: "@you:example.org"
edited_at: "2026-04-23T09:12:00Z"
received_at: "2026-04-23T09:12:00Z"
---

The corrected body.
```

The `received_at` key is kept so edit siblings are interchangeable
with originals in timeline views. `event_id` matches the original —
readers that index by `event_id` see both the original and the edit
under the same anchor.

**Scope.** This document defines the edit sibling schema. The editor
that writes these files is out of scope for
`task-20260422-1992`.

## Field reference

| Field | Type | Tier 1 | Tier 2 | Tier 3 | Edit | Notes |
|---|---|:---:|:---:|:---:|:---:|---|
| `source` | string | ✓ | ✓ | ✓ | ✓ | Ingress substrate: `matrix`, `capture`, `edit`, … |
| `authored_via` | enum | ✓ | ✓ | ✓ | ✓ | Authorship modality. Missing ⇒ `typed`. |
| `event_id` | string | ✓ | ✓ | ✓ | ✓ | Matrix event id (or cosmon anchor for `edit`). |
| `received_at` | string (RFC-3339) | ✓ | ✓ | ✓ | ✓ | Cosmon-side receipt timestamp. |
| `sender_mxid` | string | ✓ | ✓ | ✓ | — | MXID when `source == "matrix"`. |
| `sender_nucleon_id` | string | ✓ | ✓ | ✓ | — | Resolved cosmon `NucleonId`. |
| `origin_server_ts` | i64 (ms) | ✓ | ✓ | ✓ | — | Homeserver-stamped epoch millis. |
| `room_id` | string | ✓ | ✓ | ✓ | — | Matrix room id. |
| `msgtype` | string | ✓ | ✓ | ✓ | — | `m.text` (Tier 1) or `m.audio` (Tier 2/3). |
| `mxc_url` | string | — | ✓ | ✓ | — | Matrix blob URI. Kept even post-archive (Einstein). |
| `audio_sha256` | string | — | ✓ | ✓ | — | Hex content hash of the blob. |
| `audio_bytes_path` | string | — | ✓ | ✓ | — | Relative path to archived blob. |
| `duration_sec` | u32 | — | ✓ | ✓ | — | Homeserver-reported clip duration. |
| `transcription_model` | string | — | — | ✓ | — | `vendor@version`. |
| `transcription_timestamp` | string (RFC-3339) | — | — | ✓ | — | When the STT pass ran. |
| `transcription_segments` | array | — | — | ✓ | — | Per-span breakdown; always present when Tier 3. |
| `edit_of` | string | — | — | — | ✓ | Path (relative to repo) of the edited whisper. |
| `edited_by` | string | — | — | — | ✓ | MXID / principal that authored the edit. |
| `edited_at` | string (RFC-3339) | — | — | — | ✓ | Timestamp of the edit. |

**`transcription_segments` element shape:**

| Field | Type | Notes |
|---|---|---|
| `start` | f64 | Seconds from clip start. |
| `end` | f64 | Seconds from clip start. |
| `text` | string | Transcribed text for this span. |
| `avg_logprob` | f64 | Whisper-model segment-level average log-probability. Close to 0 ≡ confident; very negative ≡ low confidence. |

## Fields explicitly rejected

Proposals from the deliberation that did **not** make it into the
schema:

- `transcription_confidence` (scalar mean) — Einstein: "thermometer
  reading room temperature to summarize a fire." Use per-segment
  confidence in `transcription_segments` instead.
- `codec` / `sample_rate_hz` — derivable from the archived blob via
  `ffprobe`. Denormalization without clear value.

## Rust type reference

The canonical types live in `crates/cosmon-matrix-tick/src/inbox.rs`:

- `AuthoredVia` — snake_case enum (`Typed`, `Untranscribed`,
  `Dictated`, `Transcribed`, `Edited`); serde-derived; `Typed` is
  the default.
- `WhisperFrontmatter` — owned struct, full schema, used for parse /
  roundtrip by any third-party reader (macOS pilot, vault-editor,
  enrichment passes).
- `InboxFrontmatter<'a>` — borrow-heavy struct used by the inline
  writer in `admission::matrix_event_to_spark`. Carries an
  `authored_via: AuthoredVia` + optional `audio` and
  `transcription` references. The writer emits the YAML manually
  (the renderer is hand-rolled — `WhisperFrontmatter` is not on
  the hot path).
- `InboxTranscription<'a>` + `TranscriptionSegment<'a>` — borrow-heavy
  Tier 3 payload.

## Roundtrip contract

Every schema change must preserve:

1. A pre-schema Tier 1 whisper (no `authored_via` key) parses as
   `AuthoredVia::Typed`.
2. Every `AuthoredVia` variant roundtrips through serde without loss.
3. A Tier 3 `WhisperFrontmatter` serializes + deserializes to an
   equal value.
4. The edit sibling shape (`source: edit`, `authored_via: edited`,
   `edit_of`, `edited_by`, `edited_at`) parses as a
   `WhisperFrontmatter`.

These are tested in `inbox.rs` under `#[cfg(test)] mod tests`.

## Coherence with the architectural invariants

- **§8j (ingress bindings).** The frontmatter is emitted inside
  `matrix_event_to_spark`, so it inherits the four-clause admission
  contract. `authored_via` does not change the admission decision —
  it only records the authorship modality of an already-admitted
  event.
- **§8m (voice-message closure).** `authored_via in {dictated,
  transcribed}` + `transcription_segments` is the on-disk signal
  that §8m has been satisfied. A Tier 2 (`untranscribed`) whisper
  has admission provenance but no transcription provenance —
  readers know this at a glance without parsing the body.
- **§8e-extended (audio blob archive).** `audio_bytes_path` +
  `audio_sha256` + `mxc_url` is the triple that the blob-archive
  module populates. The schema is the contract that reads the
  archive output back.
