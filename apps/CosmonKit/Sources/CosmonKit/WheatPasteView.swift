// SPDX-License-Identifier: Apache-2.0
//
// §8k' — cross-surface wheat-paste enforcement.
//
// WheatPasteView is the only SwiftUI primitive authorised to display
// cosmon state. Every cosmon-facing surface — menubar popover,
// full-window macOS app, iPad, iPhone, Souffleur (apfel chat panel),
// Skylight (per-galaxy whisper window) — consumes the raw bytes
// emitted by `cs peek --snapshot` through this adapter.
//
// The adapter MUST NOT parse, transform, summarise, or prettify the
// input. It is the postman's letter-slot: bytes enter, glyphs appear,
// uniform stays at the door (JR §I, ADR-066 §(1)).
//
// Reference: docs/adr/066-ux-v2-substrate.md §(1) + §(5)
// Invariants: docs/architectural-invariants.md §8k' (proposed)

#if canImport(SwiftUI)
import SwiftUI

/// The only SwiftUI primitive authorised to display cosmon state.
///
/// A viewport over the canonical monospace raster emitted by
/// `cs peek --snapshot`. Wheat-pasted in place: clip, scroll, scale,
/// tint — yes. Re-render, reformat, summarise, prettify — never.
///
/// - Parameter snapshot: raw byte-stream from `cs peek --snapshot`.
///   The adapter displays the bytes verbatim in a monospaced font;
///   it does not parse them. This preserves the byte-identical raster
///   required by §8k' and the CI golden-snapshot test.
public struct WheatPasteView: View {
    /// Raw snapshot bytes emitted by `cs peek --snapshot`.
    ///
    /// The field is deliberately typed as `String` (not a parsed
    /// structure): the canon is a byte raster, not an AST. Any
    /// upstream type that implies structure (e.g. a parsed molecule
    /// list) would re-open the wheat-paste breach this adapter
    /// closes.
    public let snapshot: String

    /// Construct a viewport over the given snapshot bytes.
    public init(snapshot: String) {
        self.snapshot = snapshot
    }

    public var body: some View {
        ScrollView([.horizontal, .vertical]) {
            Text(snapshot)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
                .frame(
                    maxWidth: .infinity,
                    maxHeight: .infinity,
                    alignment: .topLeading
                )
                .padding(8)
        }
    }
}

#if DEBUG
struct WheatPasteView_Previews: PreviewProvider {
    static let fixture: String = """
    ┌─ cosmon peek ─ fleet:default ────────────────────────────────┐
    │                                                              │
    │  MOLECULE                STATUS    TEMP   ♥   WHISPER        │
    │  ──────────────────────────────────────────────────────────  │
    │  task-20260423-de93      running   ---    ▲                  │
    │  task-20260423-e49e      pending   warm   💤                  │
    │  delib-20260423-becf     completed ---    ■                   │
    │                                                              │
    └──────────────────────────────────────────────────────────────┘
    """

    static var previews: some View {
        WheatPasteView(snapshot: fixture)
            .frame(width: 640, height: 240)
    }
}
#endif

#endif
