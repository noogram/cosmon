// SPDX-License-Identifier: Apache-2.0
//
// Unit tests for the block parser feeding ``MarkdownView``. Rendering
// (SwiftUI body) is exercised by the app-level snapshot suite; these
// tests focus on the structural parse — does `# Title` become a
// heading, does `> quote` collapse consecutive lines into a
// blockquote, does a fenced block swallow the fence markers?

#if canImport(XCTest) && canImport(SwiftUI)
import XCTest
@testable import CosmonKit

final class MarkdownParserTests: XCTestCase {

    func testParsesAtxHeadings() {
        let blocks = MarkdownBlockParser.parse("# One\n## Two\n### Three")
        XCTAssertEqual(blocks, [
            .heading(level: 1, content: "One"),
            .heading(level: 2, content: "Two"),
            .heading(level: 3, content: "Three"),
        ])
    }

    func testFourHashesFallBackToParagraph() {
        // v0 supports H1–H3; #### should *not* become a heading.
        let blocks = MarkdownBlockParser.parse("#### deeper")
        XCTAssertEqual(blocks, [.paragraph("#### deeper")])
    }

    func testParagraphKeepsInlineMarkers() {
        let blocks = MarkdownBlockParser.parse("hello **world** and `code`")
        XCTAssertEqual(blocks, [.paragraph("hello **world** and `code`")])
    }

    func testMultilineParagraphJoinsLines() {
        let src = "line one\nline two\n\nline three"
        let blocks = MarkdownBlockParser.parse(src)
        XCTAssertEqual(blocks, [
            .paragraph("line one\nline two"),
            .paragraph("line three"),
        ])
    }

    func testBulletList() {
        let blocks = MarkdownBlockParser.parse("- one\n- two\n- three")
        XCTAssertEqual(blocks, [.bulletList(["one", "two", "three"])])
    }

    func testOrderedList() {
        let blocks = MarkdownBlockParser.parse("1. first\n2. second\n3. third")
        XCTAssertEqual(blocks, [.orderedList(["first", "second", "third"])])
    }

    func testBlockquoteConsumesContiguousLines() {
        let blocks = MarkdownBlockParser.parse("> line a\n> line b\n\nparagraph")
        XCTAssertEqual(blocks, [
            .blockquote(["line a", "line b"]),
            .paragraph("paragraph"),
        ])
    }

    func testFencedCodeBlockPreservesContent() {
        let src = "```\nfn main() {}\nlet x = 1;\n```"
        let blocks = MarkdownBlockParser.parse(src)
        XCTAssertEqual(blocks, [.codeBlock("fn main() {}\nlet x = 1;")])
    }

    func testHorizontalRule() {
        let blocks = MarkdownBlockParser.parse("above\n\n---\n\nbelow")
        XCTAssertEqual(blocks, [
            .paragraph("above"),
            .horizontalRule,
            .paragraph("below"),
        ])
    }

    func testGracefulOnUnclosedBold() {
        // An Inbox topic ending with `**partial` must still parse as a
        // paragraph — the renderer's inline layer catches the parse
        // failure and falls back to raw text.
        let blocks = MarkdownBlockParser.parse("start **unfinished")
        XCTAssertEqual(blocks, [.paragraph("start **unfinished")])
    }

    func testMixedDocumentPreservesOrder() {
        let src = """
        # Mission

        **Bold lead** paragraph with `code`.

        - item one
        - item two

        > Citation.

        ```
        cargo test
        ```
        """
        let blocks = MarkdownBlockParser.parse(src)
        XCTAssertEqual(blocks, [
            .heading(level: 1, content: "Mission"),
            .paragraph("**Bold lead** paragraph with `code`."),
            .bulletList(["item one", "item two"]),
            .blockquote(["Citation."]),
            .codeBlock("cargo test"),
        ])
    }
}
#endif
