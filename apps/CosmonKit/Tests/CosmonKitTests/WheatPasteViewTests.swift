// SPDX-License-Identifier: Apache-2.0
//
// Swift-side determinism test for WheatPasteView.
//
// The primary golden-snapshot test for cross-surface canon lives in
// Rust at `tests/cross_surface_canon.rs` — it exercises the byte
// raster emitted by `cs peek` and is where CI enforces §8k'. This
// Swift test asserts the narrower property that `WheatPasteView`
// holds the input bytes without mutation: the adapter is a letter
// slot, not a parser.

#if canImport(XCTest)
import XCTest
@testable import CosmonKit

final class WheatPasteViewTests: XCTestCase {
    /// The adapter holds snapshot bytes verbatim — no trimming, no
    /// normalisation, no lossy transcoding.
    func testSnapshotBytesAreVerbatim() {
        let fixture = "┌─ peek ─┐\n│ task-x │\n└────────┘\n"
        let view = WheatPasteView(snapshot: fixture)
        XCTAssertEqual(view.snapshot, fixture)
    }

    /// Constructing the view is deterministic — same input, same
    /// stored bytes.
    func testConstructionIsDeterministic() {
        let fixture = "line 1\nline 2\n"
        let a = WheatPasteView(snapshot: fixture)
        let b = WheatPasteView(snapshot: fixture)
        XCTAssertEqual(a.snapshot, b.snapshot)
    }

    /// Non-ASCII bytes survive the round trip (box-drawing glyphs,
    /// the temperature emoji, the heart ♥ pastille).
    func testNonAsciiBytesPreserved() {
        let fixture = "🔥 ♥ ■ ▲ 💤 ─ │ ┌ ┐ └ ┘\n"
        let view = WheatPasteView(snapshot: fixture)
        XCTAssertEqual(view.snapshot, fixture)
    }
}
#endif
