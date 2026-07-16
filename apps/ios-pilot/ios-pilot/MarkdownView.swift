// SPDX-License-Identifier: Apache-2.0
//
// MarkdownView — SwiftUI-native markdown renderer shared by mac-pilot,
// ios-pilot, and any future cosmon-facing viewport. v0 uses Apple's
// `AttributedString(markdown:)` for inline formatting (bold, italic,
// code, link) and a hand-rolled block splitter for the common block
// constructs:
//
//   - ATX headings (`#`, `##`, `###`)
//   - Fenced code blocks (```) — language hint ignored in v0
//   - Blockquotes (`>`)
//   - Bullet and ordered lists (only the first level in v0)
//   - Horizontal rule (`---`)
//   - Plain paragraphs (inline markdown rendered via AttributedString)
//
// Scope guards (mission brief, §4):
//   - Zero external dependencies (AttributedString is in the SDK).
//   - Graceful fallback: if inline markdown parsing fails, render the
//     raw text — the Inbox should never go blank because of a runaway
//     asterisk.
//   - Byte-identical rendering Mac ↔ iOS ↔ iPad on the same theme: the
//     only platform-specific branch is the SwiftUI View body itself,
//     which composes the same primitives on every target.
//
// Reference: docs/guides/markdown-rendering.md, ADR-066 §(1).

#if canImport(SwiftUI)
import SwiftUI

/// A SwiftUI markdown renderer governed by a ``MarkdownTheme``.
///
/// Swap this for every `Text(rawMarkdown)` site in the cosmon pilot
/// apps. The renderer parses the minimum useful subset of markdown
/// (headings, emphasis, code, links, lists, blockquotes, horizontal
/// rules, fenced code blocks) and lays it out as a `VStack` of typed
/// SwiftUI primitives. Anything unrecognised falls through to a plain
/// paragraph — no crash, no lost text.
public struct MarkdownView: View {
    /// Raw markdown source — rendered as-is with theme styling applied.
    public let text: String
    /// Theme to apply. Defaults to ``MarkdownTheme/relaxed``.
    public let theme: MarkdownTheme

    public init(text: String, theme: MarkdownTheme = .relaxed) {
        self.text = text
        self.theme = theme
    }

    public var body: some View {
        let blocks = MarkdownBlockParser.parse(text)
        return VStack(alignment: .leading, spacing: theme.blockSpacing) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                renderedBlock(block)
            }
        }
        .foregroundStyle(theme.foregroundColor)
    }

    /// One block → one SwiftUI view, themed via `self.theme`.
    @ViewBuilder
    private func renderedBlock(_ block: MarkdownBlock) -> some View {
        switch block {
        case .heading(let level, let content):
            headingView(level: level, content: content)
        case .paragraph(let content):
            inlineText(content)
                .fixedSize(horizontal: false, vertical: true)
        case .codeBlock(let content):
            codeBlockView(content)
        case .blockquote(let lines):
            blockquoteView(lines)
        case .bulletList(let items):
            listView(items: items, ordered: false)
        case .orderedList(let items):
            listView(items: items, ordered: true)
        case .horizontalRule:
            Rectangle()
                .fill(theme.mutedColor.opacity(0.35))
                .frame(height: 1)
        }
    }

    private func headingView(level: Int, content: String) -> some View {
        let size: CGFloat = {
            switch level {
            case 1:  return theme.h1Size
            case 2:  return theme.h2Size
            default: return theme.h3Size
            }
        }()
        return inlineText(content)
            .font(.system(size: size, weight: .semibold))
            .foregroundStyle(theme.accentColor)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func codeBlockView(_ content: String) -> some View {
        Text(content)
            .font(theme.codeFont)
            .foregroundStyle(theme.codeForeground)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
            .background(
                RoundedRectangle(cornerRadius: 4)
                    .fill(theme.codeBackground)
            )
    }

    private func blockquoteView(_ lines: [String]) -> some View {
        let joined = lines.joined(separator: "\n")
        return HStack(spacing: 0) {
            Rectangle()
                .fill(theme.blockquoteColor)
                .frame(width: 3)
            inlineText(joined)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, theme.blockquotePadding)
                .padding(.vertical, 4)
        }
        .background(theme.blockquoteBackground)
        .cornerRadius(2)
    }

    private func listView(items: [String], ordered: Bool) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(items.enumerated()), id: \.offset) { idx, item in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(ordered ? "\(idx + 1)." : "•")
                        .font(theme.bodyFont)
                        .foregroundStyle(theme.accentColor)
                        .monospacedDigit()
                    inlineText(item)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    /// Inline rendering via `AttributedString(markdown:)` (bold, italic,
    /// `code`, [link](...)). On parse failure — e.g. a topic with
    /// an unmatched `**` — fall back to the raw string so the Inbox
    /// row never renders blank.
    private func inlineText(_ content: String) -> Text {
        let attributed = MarkdownInline.render(content, theme: theme)
        return Text(attributed)
            .font(theme.bodyFont)
    }
}

/// Helper that turns a single inline string into an `AttributedString`
/// styled with the caller's theme. Extracted into its own type so the
/// fallback path (raw text on parse failure) is testable in isolation.
enum MarkdownInline {
    static func render(_ source: String, theme: MarkdownTheme) -> AttributedString {
        // v0 uses the system parser. `inlineOnlyPreservingWhitespace`
        // tells AttributedString to preserve `\n` inside a paragraph
        // (needed for blockquotes with explicit line breaks) while
        // still honouring bold/italic/code spans.
        let options = AttributedString.MarkdownParsingOptions(
            allowsExtendedAttributes: true,
            interpretedSyntax: .inlineOnlyPreservingWhitespace
        )
        guard var attr = try? AttributedString(markdown: source, options: options) else {
            return AttributedString(source)
        }
        applyTheme(to: &attr, theme: theme)
        return attr
    }

    /// Walk the attributed runs and apply theme colours / fonts.
    ///
    /// Parameters the stdlib parser exposes as `InlinePresentationIntent`
    /// (bold, italic, code, strikethrough) drive our theming; `link`
    /// attributes drive the colour for URL spans.
    private static func applyTheme(to attr: inout AttributedString, theme: MarkdownTheme) {
        for run in attr.runs {
            let range = run.range
            if let intent = run.inlinePresentationIntent {
                if intent.contains(.code) {
                    attr[range].font = theme.codeFont
                    attr[range].backgroundColor = theme.codeBackground
                    attr[range].foregroundColor = theme.codeForeground
                }
                if intent.contains(.emphasized) {
                    attr[range].font = theme.bodyFont.italic()
                }
                if intent.contains(.stronglyEmphasized) {
                    attr[range].font = theme.bodyFont.bold()
                }
            }
            if run.link != nil {
                attr[range].foregroundColor = theme.linkColor
                attr[range].underlineStyle = .single
            }
        }
    }
}

// MARK: - Block parser

/// Enumerated block-level constructs recognised by ``MarkdownBlockParser``.
/// Strings are raw inline-markdown — they still contain `**bold**`,
/// ``` `code` ```, `[link](...)` markers which the inline stage then
/// parses with `AttributedString(markdown:)`.
enum MarkdownBlock: Equatable {
    case heading(level: Int, content: String)
    case paragraph(String)
    case codeBlock(String)
    case blockquote([String])
    case bulletList([String])
    case orderedList([String])
    case horizontalRule
}

/// Parse the supported block subset into a sequence of `MarkdownBlock`.
///
/// The parser is deliberately simple:
///   - line-oriented, single pass
///   - no nested lists (first level only)
///   - no reference-style links (pipeline handles inline only)
///   - empty line = paragraph break
///
/// Anything the parser doesn't recognise becomes a `.paragraph` with
/// the raw text preserved, so arbitrary topic strings always have
/// *some* rendering.
enum MarkdownBlockParser {
    static func parse(_ source: String) -> [MarkdownBlock] {
        let lines = source.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        var blocks: [MarkdownBlock] = []
        var i = 0
        while i < lines.count {
            let line = lines[i]
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if trimmed.isEmpty {
                i += 1
                continue
            }

            if trimmed.hasPrefix("```") {
                // Fenced code block — consume until the matching fence.
                i += 1
                var content: [String] = []
                while i < lines.count && !lines[i].trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                    content.append(lines[i])
                    i += 1
                }
                if i < lines.count { i += 1 } // skip closing fence
                blocks.append(.codeBlock(content.joined(separator: "\n")))
                continue
            }

            if let level = headingLevel(trimmed) {
                let content = String(trimmed.drop(while: { $0 == "#" }))
                    .trimmingCharacters(in: .whitespaces)
                blocks.append(.heading(level: level, content: content))
                i += 1
                continue
            }

            if isHorizontalRule(trimmed) {
                blocks.append(.horizontalRule)
                i += 1
                continue
            }

            if trimmed.hasPrefix(">") {
                var quoteLines: [String] = []
                while i < lines.count {
                    let t = lines[i].trimmingCharacters(in: .whitespaces)
                    if !t.hasPrefix(">") { break }
                    let rest = String(t.dropFirst())
                        .trimmingCharacters(in: .whitespaces)
                    quoteLines.append(rest)
                    i += 1
                }
                blocks.append(.blockquote(quoteLines))
                continue
            }

            if isBulletListItem(trimmed) {
                var items: [String] = []
                while i < lines.count {
                    let t = lines[i].trimmingCharacters(in: .whitespaces)
                    if !isBulletListItem(t) { break }
                    items.append(bulletListItemContent(t))
                    i += 1
                }
                blocks.append(.bulletList(items))
                continue
            }

            if isOrderedListItem(trimmed) {
                var items: [String] = []
                while i < lines.count {
                    let t = lines[i].trimmingCharacters(in: .whitespaces)
                    if !isOrderedListItem(t) { break }
                    items.append(orderedListItemContent(t))
                    i += 1
                }
                blocks.append(.orderedList(items))
                continue
            }

            // Default — consume one paragraph (consecutive non-empty,
            // non-structural lines).
            var paraLines: [String] = [line]
            i += 1
            while i < lines.count {
                let t = lines[i].trimmingCharacters(in: .whitespaces)
                if t.isEmpty { break }
                if t.hasPrefix("#") && headingLevel(t) != nil { break }
                if t.hasPrefix(">") { break }
                if t.hasPrefix("```") { break }
                if isHorizontalRule(t) { break }
                if isBulletListItem(t) { break }
                if isOrderedListItem(t) { break }
                paraLines.append(lines[i])
                i += 1
            }
            blocks.append(.paragraph(paraLines.joined(separator: "\n")))
        }
        return blocks
    }

    /// 1 / 2 / 3 → heading level; 0 or >3 → nil. Requires at least one
    /// space between the hashes and the content.
    private static func headingLevel(_ trimmed: String) -> Int? {
        var hashes = 0
        for ch in trimmed where ch == "#" {
            hashes += 1
            if hashes > 3 { return nil }
        }
        guard hashes >= 1, hashes <= 3 else { return nil }
        // must be followed by a space OR be the whole line
        let afterHashes = trimmed.dropFirst(hashes)
        if afterHashes.isEmpty { return hashes }
        return afterHashes.first == " " ? hashes : nil
    }

    private static func isHorizontalRule(_ t: String) -> Bool {
        guard t.count >= 3 else { return false }
        return t.allSatisfy { $0 == "-" }
            || t.allSatisfy { $0 == "*" }
            || t.allSatisfy { $0 == "_" }
    }

    private static func isBulletListItem(_ t: String) -> Bool {
        t.hasPrefix("- ") || t.hasPrefix("* ") || t.hasPrefix("+ ")
    }

    private static func bulletListItemContent(_ t: String) -> String {
        String(t.dropFirst(2))
    }

    private static func isOrderedListItem(_ t: String) -> Bool {
        // Minimal: digit(s) followed by ". "
        var idx = t.startIndex
        var seenDigit = false
        while idx < t.endIndex, t[idx].isNumber {
            seenDigit = true
            idx = t.index(after: idx)
        }
        guard seenDigit, idx < t.endIndex, t[idx] == "." else { return false }
        let after = t.index(after: idx)
        return after < t.endIndex && t[after] == " "
    }

    private static func orderedListItemContent(_ t: String) -> String {
        if let dot = t.firstIndex(of: "."),
           t.index(after: dot) < t.endIndex {
            return String(t[t.index(dot, offsetBy: 2)...])
        }
        return t
    }
}

// MARK: - Truncation helper

public extension MarkdownView {
    /// Trim `raw` down to at most `maxChars` characters without cutting
    /// inside a markdown token — e.g. `**bold**` won't be sliced in the
    /// middle of the opening delimiter and left visibly broken in the
    /// Inbox.
    ///
    /// The heuristic is conservative: if the cut lands inside `**`,
    /// `` ` ``, `[` or `(`, we step back until we're on safe ground.
    /// A trailing ellipsis is appended when the string was actually
    /// shortened.
    static func truncatedMarkdown(_ raw: String, maxChars: Int) -> String {
        guard raw.count > maxChars, maxChars > 1 else { return raw }
        var idx = raw.index(raw.startIndex, offsetBy: maxChars)
        let unsafe: Set<Character> = ["*", "_", "`", "[", "(", "!"]
        while idx > raw.startIndex {
            let previous = raw.index(before: idx)
            if unsafe.contains(raw[previous]) {
                idx = previous
            } else {
                break
            }
        }
        return String(raw[..<idx]) + "…"
    }
}

#if DEBUG
struct MarkdownView_Previews: PreviewProvider {
    static let sample = """
    # Titre principal

    Paragraphe avec **gras**, *italique* et `code inline`.
    Lien : [cosmon](https://example.com)

    ## Sous-titre

    > Citation d'Obsidian — une baguette magique passée sur une page grise.

    ```
    fn main() {
        println!("hello world");
    }
    ```

    - premier point
    - deuxième point
    - troisième point

    1. premier
    2. deuxième

    ---
    """

    static var previews: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                MarkdownView(text: sample, theme: .obsidianDark)
                Divider()
                MarkdownView(text: sample, theme: .obsidianLight)
                Divider()
                MarkdownView(text: sample, theme: .relaxed)
                Divider()
                MarkdownView(text: sample, theme: .compact)
            }
            .padding()
        }
        .frame(width: 520)
    }
}
#endif

#endif
