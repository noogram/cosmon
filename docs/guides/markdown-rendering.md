# Markdown rendering — Swift pilot surfaces

Scope: mac-pilot + ios-pilot. Source of truth lives in
`apps/CosmonKit/Sources/CosmonKit/Markdown{View,Theme}.swift` and is
symlinked into each app's source tree so both builds ship the exact
same bytes and both Xcode projects can compile without an SPM-wiring
dance.

## Why this exists

Before task-20260423-d9a4, molecule topics and Matrix whisper bodies
were rendered via plain `Text(raw)` — you'd see `**bold**` with the
asterisks, `` `code` `` with backticks, `[link](url)` as literal text.
The operator asked for Obsidian-style styled rendering; this is the
implementation.

## Public surface

Two types, both behind `#if canImport(SwiftUI)`:

```swift
public struct MarkdownView: View {
    public let text: String
    public let theme: MarkdownTheme
    public init(text: String, theme: MarkdownTheme = .relaxed)
}

public struct MarkdownTheme: Hashable, Sendable {
    public let bodyFont: Font
    public let codeFont: Font
    public let h1Size, h2Size, h3Size: CGFloat
    public let accentColor, codeBackground, codeForeground: Color
    public let linkColor: Color
    public let blockquoteColor, blockquoteBackground: Color
    public let blockquotePadding, blockSpacing: CGFloat
    public let foregroundColor, mutedColor: Color
}
```

Four canonical themes:

| Theme            | Use-case                                         |
|------------------|--------------------------------------------------|
| `.obsidianDark`  | Obsidian-inspired dark palette (future dark mode) |
| `.obsidianLight` | Obsidian-inspired light palette                   |
| `.compact`       | Inbox list rows, whisper previews (dense)         |
| `.relaxed`       | Molecule detail, whisper body (airier)            |

Stable round-trip through `UserDefaults` via `MarkdownThemeID` (raw
strings `obsidian-dark` / `obsidian-light` / `compact` / `relaxed`).

## Where it's wired

| Surface                                     | Theme (default)               |
|---------------------------------------------|-------------------------------|
| ios-pilot Inbox list topic                  | `.compact` (fixed)            |
| ios-pilot Inbox detail (`MoleculeDetailView` topic block) | operator choice (via `SettingsStore.markdownTheme`) |
| ios-pilot Whispers list preview             | `.compact` (fixed)            |
| ios-pilot Whisper detail body               | operator choice               |
| mac-pilot Inbox detail (topic block)        | operator choice (`@AppStorage`) |
| mac-pilot Whispers list preview             | `.compact` (fixed)            |
| mac-pilot Whisper detail body               | operator choice               |

List-row renderings use `.compact` **regardless** of the operator's
theme choice — dense rows must stay legible no matter what the
Obsidian picker says. Only the detail panes follow the picker.

The iOS and Mac apps share the `@AppStorage`/`UserDefaults` key
`"markdown_theme"`, so a MacBook + iPad operator sees the same theme
on both devices the moment iCloud replicates `UserDefaults`.

## Scope-guards (v0)

- **Zero external dependencies.** We use Apple's
  `AttributedString(markdown:)` for inline formatting and a 200-line
  hand-rolled block splitter for headings/lists/blockquotes/code
  fences. No `swift-markdown-ui`, no `Down`, no CommonMark fork.
- **Four themes, not file-driven.** v1 adds a `MarkdownTheme.toml`
  if the need is proven; v0 keeps the palette locked.
- **Fallback graceful.** `AttributedString(markdown:)` can throw on
  some malformed inputs (unterminated `**`, nested `(` etc.). The
  renderer catches the failure and falls back to the raw string —
  the Inbox row never shows blank.
- **Byte-identical cross-surface.** The same theme on Mac + iPhone +
  iPad produces the same SwiftUI view tree (modulo the platform's
  native font metrics). Operationally this is §8k' wheat-paste for
  a subset of the content channel; the future cs peek byte raster
  remains untouched.

## Extending — v1 integration paths

### Syntax-highlighted code blocks

Add [`swift-markdown-ui`](https://github.com/gonzalezreal/swift-markdown-ui)
as an opt-in SwiftPM dependency behind a compile-time flag. The
`codeBlockView(_:)` branch in `MarkdownView` becomes a wrapper
around `MarkdownUI.CodeBlock` when the flag is on, and stays on the
hand-rolled renderer otherwise. Keeps the default build footprint
zero.

### Obsidian `.css` imports

A small parser would read a subset of Obsidian's CSS variables
(`--text-accent`, `--background-primary`, `--text-normal`, …) and
produce a `MarkdownTheme` struct. Candidate path:
`CosmonKit/Sources/CosmonKit/ObsidianThemeImporter.swift`, bounded
to a handful of CSS custom properties.

### Embedded inline images

`![alt](data:image/png;base64,...)` tokens. Extend the block parser
with an image-block variant and render via SwiftUI's `AsyncImage` for
URL sources or decoded `Data` for `data:` URIs.

## ios-pilot drift note

While wiring this up we discovered that main already shipped
several broken features that predated markdown rendering:

- `apps/ios-pilot/ios-pilot/MotionView.swift` references
  `MotionSnapshot`, `MotionWorker`, `MotionMolecule`, `MotionCommit`,
  `MotionWhisper`, `MotionSpark` and `api.motion(window:)` — none of
  which are defined in ios-pilot. The file is not referenced
  anywhere else in the app.
- `CosmonAPIProtocol` is missing `tackleMolecule`, `tagMolecule`,
  and `fetchCluster` — all called by shipped views (`InboxView`,
  `App.swift`).

Minimal unblock applied here so that `xcodebuild` on ios-pilot
succeeds:

1. `MotionView.swift` is excluded from the target via
   `apps/ios-pilot/project.yml` (xcodegen `excludes`).
2. The missing `CosmonAPIProtocol` methods gained
   `notImplemented`-throwing default extensions in
   `apps/ios-pilot/ios-pilot/CosmonAPI.swift`.

A follow-up molecule (`temp:warm`) should either port
`MotionSnapshot`+ the API path from mac-pilot/Models.swift and the
Rust cs-api `/motion` endpoint, or delete the orphan file and the
associated call sites. Whichever resolution lands, it must remove
both excludes above so ios-pilot returns to a single compile path.
