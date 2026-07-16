# Sources

Drop the article's source corpus here. The `sourcer` role builds
`source-ledger.md` from whatever lives in this directory.

Accepted inputs:
- **PDFs** — peer-reviewed papers, book chapters, reputable reports.
- **Markdown notes** (`*.md`) — already-structured source digests.
- **URL list** (`urls.txt`) — one URL per line; sourcer resolves each.
- **BibTeX** (`*.bib`) — pre-resolved citation metadata.

Reliability tiers (sourcer assigns):
- **tier-1** — peer-reviewed secondary (reviews, textbooks, survey articles).
- **tier-2** — reputable secondary (major-press books, established encyclopedias).
- **tier-3** — primary research (original experiments, preprints).
- **tier-4** — self-published / blogs (avoid; auto-flagged).

Policy (see `MISSION.md` frontmatter `[sources].policy`):
- `secondary-preferred` (default) — ≥70% tier-1/2. Pure-primary triggers a
  `secondary-source-gap.md` report instead of proceeding.
- `primary-allowed` — primary sources are first-class (use for bleeding-edge
  research articles where no secondary exists yet).
- `medical-medrs` — MEDRS-style: tier-1 only; reviews over primary trials.

Delete this README once you've dropped real sources (it is not read by
the fleet).
