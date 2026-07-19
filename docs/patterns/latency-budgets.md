# Latency Budgets per Domain

Cosmon's rigour sits on top of latency budgets that differ by ~6 orders of
magnitude between application domains. The discipline that ships in voice
(500 ms turn start) would be catastrophic overhead in HFT colo (100 μs
order placement). This table freezes what is **forbidden on the hot path**
of each domain so that architectural reviews can point at a violation
without debate — "fsync is a 100 μs–10 ms event; it cannot live on the
HFT colo path" closes the discussion.

The table lives here, not in a synthesis, because budgets are a standing
reference that every cross-galaxy design decision pulls against. When a
trait, crate, or primitive is proposed as *portable*, the first test is:
does it respect the tightest budget of its target domains?

## Budgets

| Domain | p99 budget | Forbidden on hot path | Source |
|--------|-----------:|-----------------------|--------|
| **Voice** | 500 ms turn start | — (anything goes under tokio) | `voix/docs/adr/0003-cosmon-voice-bridge.md §1`; 200 ms human threshold |
| **Stage (RT callback)** | 2.67 ms callback / 5 ms pad-to-sound | alloc, mutex, syscall, scheduler hop | `showroom/docs/adr/001-audio-backend.md §1, §2.4` |
| **HFT colo** | 100 μs order placement | tokio, `serde_json`, `dyn`, fsync, cross-core hop | operator-given (provisional) |
| **HFT Mac non-colo** | 10 ms | fsync (~100 μs–10 ms), scheduler hop | operator-given (provisional) |

## Notes

- **6 orders of magnitude between voice and HFT colo** means three distinct
  machines pretending to share a runtime (torvalds). A generic binary is
  impossible; a generic *algebra* documented in prose is fine, and it is
  the algebra that ships — not the binary.
- **Why fsync is OK in voice/stage but forbidden in HFT colo.** A warm
  fsync takes ~100 μs–10 ms. That fits comfortably in voice's 500 ms and
  stage's 2.67 ms if pre-allocated on the audio thread's I/O sibling, but
  it is 1–100× the HFT colo budget. The same primitive (durable append)
  ships in voice/stage and gets **replaced** in HFT — same algebra,
  different substrate (shm-ring, no fsync).
- **Why `dyn` is OK in voice but forbidden in HFT colo.** Indirect-call
  overhead is 2–5% per dispatch; imperceptible against a 500 ms budget,
  fatal at 100 μs. Trait objects belong in slow-path plumbing, not on the
  HFT hot path.
- **The operating rule.** If a feature imposes a cost greater than the
  budget of one of its target domains, **it does not belong in the shared
  playbook**. It belongs in a domain-specific adapter. This is how the
  algebra stays portable while the binaries stay specialised.

## How to amend this table

- Any modification must cite an **empirical measurement** (bench, profile,
  production trace) — not an intuition.
- The HFT budgets are provisional. They will be re-validated by a
  dedicated HFT architecture deliberation (see §10 follow-up #5 of
  `delib-20260420-f3ef`). Until then, treat them as working estimates.
- Cited sources (ADRs, bench files) must be reproducible: link + date of
  the referenced revision. When a budget is tightened or relaxed, link
  the commit that changed it.
- Adding a new domain (e.g. RT video, embedded, batch ETL) requires a
  p99 budget with source, a "forbidden on hot path" column backed by the
  budget arithmetic, and a cross-ref to the domain's own CLAUDE.md or
  ADR index.

## Cross-refs

- `delib-20260420-f3ef/synthesis.md §6` — origin of the table.
- The *session-primitive* shape — ordered single-writer log + unique
  terminal + idempotent projection + observe-before-emit — respects these
  budgets by design. Its playbook is maintained outside this tree.
