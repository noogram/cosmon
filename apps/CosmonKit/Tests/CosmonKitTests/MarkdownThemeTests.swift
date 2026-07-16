// SPDX-License-Identifier: Apache-2.0
//
// Tests for `MarkdownTheme` and its stable-id enum. The goal is not to
// lock every colour value — those are intentionally loose — but to
// ensure the canonical variants round-trip through `MarkdownThemeID`
// (as the operator's theme choice is persisted in UserDefaults).

#if canImport(XCTest) && canImport(SwiftUI)
import XCTest
@testable import CosmonKit

final class MarkdownThemeTests: XCTestCase {

    func testEveryThemeIDResolvesToDistinctTheme() {
        let themes = MarkdownThemeID.allCases.map(\.theme)
        XCTAssertEqual(themes.count, 4)
        XCTAssertEqual(Set(themes).count, 4,
                       "The four canonical themes must differ so the picker has a visible effect.")
    }

    func testThemeIDRoundTripsThroughRawValue() {
        for id in MarkdownThemeID.allCases {
            XCTAssertEqual(MarkdownThemeID(rawValue: id.rawValue), id)
        }
    }

    func testThemeIDLabelsAreFrenchHumanReadable() {
        // Surface-level stability — settings UI copy.
        XCTAssertEqual(MarkdownThemeID.obsidianDark.label,  "Obsidian — sombre")
        XCTAssertEqual(MarkdownThemeID.obsidianLight.label, "Obsidian — clair")
        XCTAssertEqual(MarkdownThemeID.compact.label,       "Compact")
        XCTAssertEqual(MarkdownThemeID.relaxed.label,       "Relaxed")
    }

    func testCompactThemeIsTighterThanRelaxed() {
        XCTAssertLessThan(MarkdownTheme.compact.h1Size, MarkdownTheme.relaxed.h1Size)
        XCTAssertLessThan(MarkdownTheme.compact.blockSpacing, MarkdownTheme.relaxed.blockSpacing)
    }

    func testTruncationHonoursMarkdownTokenBoundary() {
        // A naive slice at 7 would land on the asterisk inside `**bold`
        // leaving `hello **`  — the helper walks backward off unsafe
        // characters until it reaches safe ground so the Inbox never
        // shows a half-open delimiter.
        let raw = "hello **bold** and more"
        let out = MarkdownView.truncatedMarkdown(raw, maxChars: 8)
        XCTAssertTrue(out.hasSuffix("…"))
        XCTAssertFalse(
            out.hasSuffix("**…") || out.hasSuffix("*…"),
            "Should not leave trailing asterisks mid-token: got \(out)"
        )
    }

    func testTruncationPassesThroughShortStrings() {
        let raw = "hello"
        XCTAssertEqual(MarkdownView.truncatedMarkdown(raw, maxChars: 50), raw)
    }
}
#endif
