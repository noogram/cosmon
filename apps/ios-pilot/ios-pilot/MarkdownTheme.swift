// SPDX-License-Identifier: Apache-2.0
//
// MarkdownTheme — typography + colour palette applied by `MarkdownView`.
//
// Themes are plain value types. Four canonical variants ship in the
// library (`obsidianDark`, `obsidianLight`, `compact`, `relaxed`); any
// caller can build a custom one by constructing a `MarkdownTheme`
// directly. There is no file-based config yet — v1 keeps the surface
// small; v2 may read `~/.cosmon/markdown-theme.toml` once the need is
// proven.
//
// Reference: docs/guides/markdown-rendering.md (design + extension
// path), ADR-066 §(1) wheat-paste — the theme is identical across
// mac-pilot, ios-pilot, and future viewports so a given topic renders
// byte-identical regardless of the surface.

#if canImport(SwiftUI)
import SwiftUI

/// Typography + colour palette applied by ``MarkdownView``.
///
/// Built to be shared across mac-pilot, ios-pilot, and any future
/// SwiftUI viewport: the same theme key on two surfaces must yield
/// the same visual output (§8k' wheat-paste). Four canonical variants
/// are provided; `custom(...)` constructors are trivial since the type
/// is a plain struct of stored properties.
public struct MarkdownTheme: Hashable, Sendable {

    /// Base font applied to paragraph text.
    public let bodyFont: Font
    /// Monospaced font used for inline `code` spans and fenced blocks.
    public let codeFont: Font
    /// Explicit heading sizes. Headings beyond H3 fall back to H3.
    public let h1Size: CGFloat
    public let h2Size: CGFloat
    public let h3Size: CGFloat
    /// Accent tint used for headings and strong emphasis.
    public let accentColor: Color
    /// Background applied to inline code and fenced code blocks.
    public let codeBackground: Color
    /// Foreground colour for fenced code blocks (inline inherits it too).
    public let codeForeground: Color
    /// Link colour (links render but are non-tappable in compact views).
    public let linkColor: Color
    /// Border colour applied to the blockquote rule.
    public let blockquoteColor: Color
    /// Background applied to blockquote blocks.
    public let blockquoteBackground: Color
    /// Horizontal padding applied around blockquote content.
    public let blockquotePadding: CGFloat
    /// Spacing between rendered blocks (paragraphs, headings, lists).
    public let blockSpacing: CGFloat
    /// Foreground for the primary prose flow.
    public let foregroundColor: Color
    /// Foreground used for muted secondary text (blockquote, hr, …).
    public let mutedColor: Color

    public init(
        bodyFont: Font,
        codeFont: Font,
        h1Size: CGFloat,
        h2Size: CGFloat,
        h3Size: CGFloat,
        accentColor: Color,
        codeBackground: Color,
        codeForeground: Color,
        linkColor: Color,
        blockquoteColor: Color,
        blockquoteBackground: Color,
        blockquotePadding: CGFloat,
        blockSpacing: CGFloat,
        foregroundColor: Color,
        mutedColor: Color
    ) {
        self.bodyFont = bodyFont
        self.codeFont = codeFont
        self.h1Size = h1Size
        self.h2Size = h2Size
        self.h3Size = h3Size
        self.accentColor = accentColor
        self.codeBackground = codeBackground
        self.codeForeground = codeForeground
        self.linkColor = linkColor
        self.blockquoteColor = blockquoteColor
        self.blockquoteBackground = blockquoteBackground
        self.blockquotePadding = blockquotePadding
        self.blockSpacing = blockSpacing
        self.foregroundColor = foregroundColor
        self.mutedColor = mutedColor
    }
}

public extension MarkdownTheme {

    /// Obsidian-inspired dark palette — purple accent, slate code block.
    /// Tuned for molecule detail panes and longer reading surfaces.
    static let obsidianDark = MarkdownTheme(
        bodyFont: .system(.body, design: .default),
        codeFont: .system(.callout, design: .monospaced),
        h1Size: 26,
        h2Size: 22,
        h3Size: 18,
        accentColor: Color(red: 0.68, green: 0.52, blue: 1.00),
        codeBackground: Color(red: 0.12, green: 0.13, blue: 0.17),
        codeForeground: Color(red: 0.92, green: 0.88, blue: 0.98),
        linkColor: Color(red: 0.55, green: 0.75, blue: 1.0),
        blockquoteColor: Color(red: 0.48, green: 0.38, blue: 0.82),
        blockquoteBackground: Color(red: 0.15, green: 0.13, blue: 0.22).opacity(0.55),
        blockquotePadding: 10,
        blockSpacing: 10,
        foregroundColor: Color(red: 0.90, green: 0.90, blue: 0.94),
        mutedColor: Color(red: 0.65, green: 0.65, blue: 0.70)
    )

    /// Obsidian-inspired light palette — purple accent, parchment code block.
    static let obsidianLight = MarkdownTheme(
        bodyFont: .system(.body, design: .default),
        codeFont: .system(.callout, design: .monospaced),
        h1Size: 26,
        h2Size: 22,
        h3Size: 18,
        accentColor: Color(red: 0.40, green: 0.25, blue: 0.85),
        codeBackground: Color(red: 0.96, green: 0.95, blue: 0.92),
        codeForeground: Color(red: 0.20, green: 0.15, blue: 0.32),
        linkColor: Color(red: 0.15, green: 0.40, blue: 0.85),
        blockquoteColor: Color(red: 0.55, green: 0.45, blue: 0.85),
        blockquoteBackground: Color(red: 0.92, green: 0.90, blue: 0.98),
        blockquotePadding: 10,
        blockSpacing: 10,
        foregroundColor: Color(red: 0.12, green: 0.12, blue: 0.18),
        mutedColor: Color(red: 0.45, green: 0.45, blue: 0.50)
    )

    /// Small, tight theme for Inbox / Whispers list rows where one or
    /// two lines of topic must render legibly at list-item density.
    static let compact = MarkdownTheme(
        bodyFont: .footnote,
        codeFont: .system(.caption, design: .monospaced),
        h1Size: 14,
        h2Size: 13,
        h3Size: 12,
        accentColor: .accentColor,
        codeBackground: Color.secondary.opacity(0.15),
        codeForeground: .primary,
        linkColor: .accentColor,
        blockquoteColor: Color.secondary.opacity(0.6),
        blockquoteBackground: Color.secondary.opacity(0.08),
        blockquotePadding: 6,
        blockSpacing: 2,
        foregroundColor: .primary,
        mutedColor: .secondary
    )

    /// Larger, airier theme for molecule detail panes and whisper body
    /// views — generous heading sizes, visible blockquote rule.
    static let relaxed = MarkdownTheme(
        bodyFont: .body,
        codeFont: .system(.callout, design: .monospaced),
        h1Size: 24,
        h2Size: 20,
        h3Size: 17,
        accentColor: .accentColor,
        codeBackground: Color.secondary.opacity(0.12),
        codeForeground: .primary,
        linkColor: .accentColor,
        blockquoteColor: Color.accentColor.opacity(0.55),
        blockquoteBackground: Color.accentColor.opacity(0.08),
        blockquotePadding: 10,
        blockSpacing: 8,
        foregroundColor: .primary,
        mutedColor: .secondary
    )
}

/// Stable identifier for a canonical `MarkdownTheme`. Round-trips
/// through `UserDefaults` (string raw value) so the operator's theme
/// choice survives app relaunches. Custom themes are not persistable
/// yet — v1 focuses on the four canonical variants.
public enum MarkdownThemeID: String, CaseIterable, Identifiable, Codable, Sendable {
    case obsidianDark   = "obsidian-dark"
    case obsidianLight  = "obsidian-light"
    case compact        = "compact"
    case relaxed        = "relaxed"

    public var id: String { rawValue }

    /// Human-readable label for the Settings picker.
    public var label: String {
        switch self {
        case .obsidianDark:  return "Obsidian — sombre"
        case .obsidianLight: return "Obsidian — clair"
        case .compact:       return "Compact"
        case .relaxed:       return "Relaxed"
        }
    }

    /// Resolve the canonical `MarkdownTheme` value for this id.
    public var theme: MarkdownTheme {
        switch self {
        case .obsidianDark:  return .obsidianDark
        case .obsidianLight: return .obsidianLight
        case .compact:       return .compact
        case .relaxed:       return .relaxed
        }
    }
}
#endif
