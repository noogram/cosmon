# wiki-halo

Bridge **diagnostic → poétique** for the operator-owned citation graph.

For every `/srv/cosmon/knowledge/wiki/*.md` entry that carries a DOI, this
script queries OpenAlex for its citation neighbourhood and labels each
neighbour:

| color  | meaning                                                |
|--------|--------------------------------------------------------|
| 🟡 gold   | already in `wiki/` (operator-owned, full materialization) |
| ⚪ silver | in Zotero but not yet promoted to a wiki entry         |
| ⚫ grey   | the *halo of the unknown* — neither in wiki/ nor Zotero |

This is **jr's C5 of ADR-091** §D9 (delib-20260509-39ad §1.5), shipped under
task-20260509-ca0a. The almanac-passport visualisation (task-20260509-4c65)
already shipped the *diagnostic* counterpart; this is the *poétique* one.

## Usage

```bash
python3 tools/wiki-halo/build_halo.py            # full run (~1 min)
python3 tools/wiki-halo/build_halo.py --smoke    # 3 entries, sanity check
python3 tools/wiki-halo/build_halo.py --limit=10 # first 10 entries
```

No external Python deps — stdlib only (`urllib`, `sqlite3`, `json`, `re`).

The script reads:

- `/srv/cosmon/knowledge/wiki/*.md` — frontmatter for citekey / DOI / arxiv
- `~/Zotero/zotero.sqlite` — direct read-only sqlite query for the
  silver-classification index (DOIs + lowercased titles)
- OpenAlex API at `https://api.openalex.org` with the polite-pool email
  configured in `~/.config/almanac/config.toml` (currently `you@noogram.dev`)

## Outputs

Written under `/srv/cosmon/knowledge/zotero-coverage/wiki-halo/<YYYY-MM-DD>/`:

| file | role |
|------|------|
| `nodes.ndjson` | one JSON node per line: `{type,oa_id,doi,citekey,color,title,year,cited_by_count}` |
| `edges.ndjson` | one JSON edge per line: `{src,dst,kind:cites}` |
| `wiki-halo.md` | human-readable summary table (counts, per-citekey detail, unscannable / 404 buckets) |
| `wiki-halo.canvas` | Obsidian Canvas spec — gold centres in a grid, halo around each |
| `run-stats.json` | timings, counts, color distribution |

## Throttling and slow-path discipline

- Polite pool only: `mailto=` query parameter on every request.
- One request every 200 ms (5 req/s; OpenAlex tolerates 10).
- Exponential backoff on 429/5xx (1, 2, 4, 8 s).
- 404s are cached and surfaced as the `not_in_openalex` bucket — never retried.
- **Never invokes Sci-Hub or any other gated slow-path.** This is metadata-only
  — the gate would be `Allow|Throttle`, not `RequireOperatorGesture`.

## Scope (V0)

This is the smallest viable artefact for the wiki-halo idea. Out of scope today:

1. **Promote silver→gold** — the silver list is the immediate authoring
   backlog; promotion is a per-paper editorial decision.
2. **Cluster grey** — densest grey neighbourhoods are highest-value
   acquisition leads (a future heuristic).
3. **Weekly delta** — re-run weekly to surface what just appeared in the halo
   (alpha decay of the citation neighbourhood).
4. **Replace the Obsidian graph view** — the canvas is a static reading aid;
   the wiki-halo idea long-term is to *enrich* Obsidian's existing graph view
   with halo coloring rather than ship a parallel viewer.

## Provenance

- ADR-091 §D9 — Wiki-halo identified as the second viz to ship after almanac-passport.
- delib-20260509-39ad §1.5 — jr's C5 verbatim spec (in/out neighbourhood,
  gold/silver/grey, mark "the halo of the unknown").
- task-20260509-ca0a — implementation.
