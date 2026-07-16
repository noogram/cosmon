# Adversarial layout review

This review covers the rendered mdBook site, not only its source styles. Every
final page was rendered to PNG and visually inspected in both calibrated themes
at 390, 1024, 1440, and 1900 pixels wide.

## Coverage

| Page | Layout exercised |
|---|---|
| Introduction | body rhythm, callout, links, overview diagram, footer navigation |
| Architecture: the two layers | detailed diagrams, long prose, nested lists, page TOC |
| Run cosmon as a remote service | new diagram, table, callout, long code blocks, scrollspy |
| Exit codes & JSON output | reference table, inline code, fenced code |
| Set up cosmon (prerequisites) | tutorial flow, three-column table, code blocks |

The final matrix contains 40 full-page captures in `visual-pass6`, plus theme
popup and bottom-of-page scrollspy captures for light and coal. Automated seam
measurements accompany the images in `metrics.json` and `interactions.json`.

## Findings and corrections

| Finding before | Correction after |
|---|---|
| The paint-brush theme control was a tall rounded pill, offset from print, GitHub, and edit. | The control is an icon-only action with the same line height, spacing, and baseline as its neighbours. All four measured centres are exactly `y = 25px` at every tested width and in both themes. |
| mdBook's native theme variables overrode the intended palette: coal inherited a light page background and light rendered white instead of sage. | Theme tokens now use `html.light` and `html.coal`, winning the cascade. Rendered backgrounds are sage `rgb(232, 239, 231)` and coal `rgb(8, 9, 12)`. |
| The native theme popup exposed uncalibrated choices. | Only Light and Coal are visible; the trigger has no text label. Both popup states were opened and inspected. |
| Syntax highlighting painted a white rectangle inside coal code blocks. | The highlighter's inner background is transparent, so code inherits the calibrated sage or dark surface while retaining syntax colour. |
| Mobile figures kept browser-default horizontal margins, shrinking detailed diagrams from the available 322 px to roughly 242 px. | Figures use the full content column. Both light and dark diagram variants remain transparent, legible, and contained. |
| Three-column tables became narrow letter fragments on phones; long tokens also risked horizontal overflow. | Table headings are attached to cells at runtime and rows become labelled cards below 620 px. Text stays readable and the document remains free of horizontal overflow. |
| The hidden mobile title collapsed to an ellipsis that looked like an extra menu action. | Its text is visually removed while the centre spacer remains, leaving only the intended controls. |
| Active navigation needed consistent proof in both columns. | The current chapter in the left sidebar and current section in “On this page” use the calibrated green. Bottom scrollspy selects “See also” with `aria-current="location"` in both themes. |

## Final visual checks

- No document or table overflow in any of the 40 final renders.
- Sidebar, content, page TOC, and footer navigation remain aligned at desktop widths.
- The mobile sidebar trigger and four right-side actions remain on one centred row.
- Callouts, code, tables, links, and headings keep readable contrast in both themes.
- Architecture and remote-service diagrams show the matching light/dark asset,
  with no opaque page-sized background, overlap, or clipping.
- Theme popup options are exactly `Light` and `Coal`.
- Scrollspy reaches the final section in light and coal.
