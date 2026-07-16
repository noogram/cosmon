# CosmonKit

Shared Swift primitives for every cosmon-facing surface.

## WheatPasteView

The sole SwiftUI primitive authorised to display cosmon state
(ADR-066 §(1) + §(5); `docs/architectural-invariants.md` §8k').

Every cosmon-facing surface — menubar popover, full-window macOS
app (Workshop), iPad, iPhone, Souffleur (apfel chat panel), Skylight
(per-galaxy whisper window), future Vision / Apple TV / e-ink /
web-mirror viewports — consumes the raw byte-stream emitted by
`cs peek --snapshot` through `WheatPasteView(snapshot:)`.

The adapter is a letter-slot, not a parser. It renders the bytes
verbatim in a monospaced font — clip, scroll, scale, tint yes;
re-render, reformat, summarise, prettify never. The CI
golden-snapshot test in `tests/cross_surface_canon.rs` (Rust
workspace level) enforces byte-identity of the canon across every
surface.

## Usage

```swift
import CosmonKit

struct MoleculePanel: View {
    let snapshot: String  // from `cs peek --snapshot --molecule ...`

    var body: some View {
        WheatPasteView(snapshot: snapshot)
    }
}
```

## Tests

Swift tests:

```bash
cd apps/CosmonKit
swift test
```

These exercise the narrower property that `WheatPasteView` holds
its input without mutation. The primary cross-surface golden test
lives in Rust (CI enforces both).

## References

- ADR-066 — UX v2 substrate (parent ADR)
- ADR-064 §C4 — wheat-paste precedent (single-surface)
- `docs/architectural-invariants.md` §8k' — the invariant
- `delib-20260423-becf` — the parent deliberation (JR §I verdict)
