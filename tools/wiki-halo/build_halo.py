#!/usr/bin/env python3
"""Wiki-halo viz — bridge diagnostic→poétique for the operator-owned citation graph.

For every wiki/ entry with a DOI, query OpenAlex for the in/out citation
neighbourhood and mark each neighbour:

  gold   — already in /srv/cosmon/knowledge/wiki/  (operator-owned, full materialization)
  silver — in Zotero but not (yet) in wiki/        (known but not yet promoted)
  grey   — neither in wiki/ nor in Zotero          (the "halo of the unknown")

Outputs land under /srv/cosmon/knowledge/zotero-coverage/wiki-halo/<date>/:
  - nodes.ndjson       (one JSON node per line: paper)
  - edges.ndjson       (one JSON edge per line: cites)
  - wiki-halo.md       (human-readable summary table)
  - wiki-halo.canvas   (Obsidian Canvas spec — gold/silver/grey color-coded)
  - run-stats.json     (timings, counts, error buckets)

Source : ADR-091 §D9 (jr's C5), delib-20260509-39ad jr verdict, task-20260509-ca0a.
"""
from __future__ import annotations

import json
import re
import sqlite3
import sys
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Iterable

WIKI_DIR = Path("/srv/cosmon/knowledge/wiki")
ZOTERO_SQLITE = Path("/Users/you/Zotero/zotero.sqlite")
OUT_DIR = Path("/srv/cosmon/knowledge/zotero-coverage/wiki-halo/2026-05-09")
POLITE_EMAIL = "dana@noogram.dev"
OPENALEX = "https://api.openalex.org"

# Throttle: OpenAlex polite pool tolerates 10 req/s; we stay at 5 req/s.
MIN_INTERVAL = 0.20  # seconds between requests
TOP_K_CITED_BY = 20  # cap in-edges per paper
USER_AGENT = f"cosmon-wiki-halo/0.1 (mailto:{POLITE_EMAIL})"

DOI_RE = re.compile(r"10\.\d{4,9}/[^\s\"<>]+")
ARXIV_RE = re.compile(r"arxiv\.org/abs/(?P<id>[\w\-\./]+?)(?:v\d+)?(?:[\s\?\&\#]|$)", re.IGNORECASE)


# ──────────────────────────────────────────────────────────────────────────
# Frontmatter extraction
# ──────────────────────────────────────────────────────────────────────────


@dataclass
class WikiEntry:
    file_path: Path
    citekey: str | None
    item_key: str | None
    doi: str | None
    openalex_id: str | None
    source_url: str | None
    title: str | None


def parse_frontmatter(path: Path) -> dict[str, str]:
    text = path.read_text(encoding="utf-8", errors="replace")
    if not text.startswith("---"):
        return {}
    end = text.find("\n---", 4)
    if end < 0:
        return {}
    block = text[4:end]
    out: dict[str, str] = {}
    for line in block.splitlines():
        m = re.match(r"^([A-Za-z_][\w-]*):\s*(.*)$", line)
        if not m:
            continue
        key, val = m.group(1), m.group(2).strip()
        if val.startswith('"') and val.endswith('"'):
            val = val[1:-1]
        if val.startswith("'") and val.endswith("'"):
            val = val[1:-1]
        out[key] = val
    return out


def extract_doi(fm: dict[str, str]) -> str | None:
    raw = fm.get("doi") or fm.get("DOI")
    if raw and raw.lower() not in {"null", "none", ""}:
        m = DOI_RE.search(raw)
        if m:
            return m.group(0).lower().rstrip(".,;)")
    src = fm.get("source") or ""
    m = DOI_RE.search(src)
    if m:
        return m.group(0).lower().rstrip(".,;)")
    # arxiv URL → synthesise OpenAlex-canonical DOI form
    m = ARXIV_RE.search(src + " ")
    if m:
        arxiv_id = m.group("id").rstrip("/")
        return f"10.48550/arxiv.{arxiv_id}".lower()
    return None


def extract_openalex_id(fm: dict[str, str]) -> str | None:
    raw = fm.get("openalex_id") or fm.get("openalex")
    if raw and raw.lower() not in {"null", "none", ""}:
        m = re.search(r"W\d+", raw)
        if m:
            return m.group(0)
    return None


def load_wiki_entries() -> list[WikiEntry]:
    out: list[WikiEntry] = []
    for p in sorted(WIKI_DIR.glob("*.md")):
        fm = parse_frontmatter(p)
        if not fm:
            continue
        ck = (fm.get("zotero_citekey") or "").strip("\"' ")
        ik = (fm.get("zotero_item_key") or "").strip("\"' ")
        if ck.lower() in {"null", "none", ""}:
            ck = ""
        if ik.lower() in {"null", "none", ""}:
            ik = ""
        out.append(
            WikiEntry(
                file_path=p,
                citekey=ck or None,
                item_key=ik or None,
                doi=extract_doi(fm),
                openalex_id=extract_openalex_id(fm),
                source_url=fm.get("source"),
                title=(fm.get("title") or "").strip("\"' ") or None,
            )
        )
    return out


# ──────────────────────────────────────────────────────────────────────────
# Zotero DOI/title index (silver classification)
# ──────────────────────────────────────────────────────────────────────────


@dataclass
class ZoteroIndex:
    dois: set[str]                  # lowercased DOIs
    titles: dict[str, str]          # lowercased title → itemKey

    def has_doi(self, doi: str) -> bool:
        return doi.lower() in self.dois

    def has_title(self, title: str, threshold: float = 0.92) -> str | None:
        t = _norm_title(title)
        if t in self.titles:
            return self.titles[t]
        # Light fuzzy: prefix-equal then jaccard of word sets
        for cand_t, key in self.titles.items():
            if _title_sim(t, cand_t) >= threshold:
                return key
        return None


def _norm_title(s: str) -> str:
    s = s.lower()
    s = re.sub(r"[^\w\s]", " ", s)
    s = re.sub(r"\s+", " ", s).strip()
    return s


def _title_sim(a: str, b: str) -> float:
    sa, sb = set(a.split()), set(b.split())
    if not sa or not sb:
        return 0.0
    return len(sa & sb) / len(sa | sb)


def load_zotero_index() -> ZoteroIndex:
    con = sqlite3.connect(f"file:{ZOTERO_SQLITE}?mode=ro", uri=True)
    cur = con.cursor()
    cur.execute(
        """
        SELECT v.value
        FROM itemDataValues v
        JOIN itemData d ON d.valueID = v.valueID
        JOIN fields f ON f.fieldID = d.fieldID
        WHERE f.fieldName = 'DOI'
        """
    )
    dois: set[str] = set()
    for (val,) in cur.fetchall():
        m = DOI_RE.search(str(val))
        if m:
            dois.add(m.group(0).lower().rstrip(".,;)"))
    cur.execute(
        """
        SELECT i.key, v.value
        FROM items i
        JOIN itemData d ON d.itemID = i.itemID
        JOIN fields f ON f.fieldID = d.fieldID
        JOIN itemDataValues v ON v.valueID = d.valueID
        WHERE f.fieldName = 'title'
        """
    )
    titles: dict[str, str] = {}
    for key, val in cur.fetchall():
        if not val:
            continue
        titles[_norm_title(str(val))] = str(key)
    con.close()
    return ZoteroIndex(dois=dois, titles=titles)


# ──────────────────────────────────────────────────────────────────────────
# OpenAlex client (throttled, caching, backoff)
# ──────────────────────────────────────────────────────────────────────────


class OpenAlex:
    def __init__(self, polite_email: str = POLITE_EMAIL):
        self.polite_email = polite_email
        self._last_call = 0.0
        self._cache: dict[str, dict] = {}

    def _throttle(self):
        elapsed = time.monotonic() - self._last_call
        if elapsed < MIN_INTERVAL:
            time.sleep(MIN_INTERVAL - elapsed)
        self._last_call = time.monotonic()

    def _get(self, url: str) -> dict | None:
        if url in self._cache:
            return self._cache[url]
        self._throttle()
        sep = "&" if "?" in url else "?"
        full = f"{url}{sep}mailto={self.polite_email}"
        backoff = 1.0
        for attempt in range(4):
            try:
                req = urllib.request.Request(full, headers={"User-Agent": USER_AGENT})
                with urllib.request.urlopen(req, timeout=15) as resp:
                    raw = resp.read()
                    data = json.loads(raw)
                    self._cache[url] = data
                    return data
            except urllib.error.HTTPError as e:
                if e.code == 404:
                    self._cache[url] = None  # type: ignore[assignment]
                    return None
                if e.code in (429, 500, 502, 503, 504):
                    time.sleep(backoff)
                    backoff *= 2
                    continue
                self._cache[url] = None  # type: ignore[assignment]
                return None
            except (urllib.error.URLError, TimeoutError, OSError):
                time.sleep(backoff)
                backoff *= 2
        self._cache[url] = None  # type: ignore[assignment]
        return None

    def work_by_doi(self, doi: str) -> dict | None:
        return self._get(f"{OPENALEX}/works/https://doi.org/{urllib.parse.quote(doi)}")

    def work_by_id(self, oa_id: str) -> dict | None:
        return self._get(f"{OPENALEX}/works/{oa_id}")

    def works_filter(self, *, filter_str: str, per_page: int = 25, sort: str | None = None) -> dict | None:
        url = f"{OPENALEX}/works?filter={urllib.parse.quote(filter_str)}&per-page={per_page}"
        if sort:
            url += f"&sort={urllib.parse.quote(sort)}"
        return self._get(url)


def _doi_of(work: dict) -> str | None:
    raw = work.get("doi") or ""
    m = DOI_RE.search(raw or "")
    return m.group(0).lower().rstrip(".,;)") if m else None


def _oa_id_of(work: dict) -> str | None:
    raw = work.get("id") or ""
    m = re.search(r"W\d+", raw)
    return m.group(0) if m else None


# ──────────────────────────────────────────────────────────────────────────
# Halo build
# ──────────────────────────────────────────────────────────────────────────


@dataclass
class Node:
    oa_id: str | None
    doi: str | None
    title: str
    color: str             # gold | silver | grey
    citekey: str | None    # filled if gold or silver
    year: int | None
    cited_by_count: int | None

    def key(self) -> str:
        return self.oa_id or (self.doi or "") or self.title


@dataclass
class Edge:
    src: str          # node key (oa_id|doi|title)
    dst: str          # node key
    kind: str         # "cites" (src cites dst) or "cited_by" (src is cited by dst)


def classify(work: dict, wiki_by_doi: dict[str, WikiEntry], zot: ZoteroIndex) -> tuple[str, str | None]:
    doi = _doi_of(work)
    title = work.get("display_name") or work.get("title") or ""
    if doi and doi in wiki_by_doi:
        we = wiki_by_doi[doi]
        return "gold", we.citekey or we.item_key
    if doi and zot.has_doi(doi):
        return "silver", None
    if title:
        match = zot.has_title(title)
        if match:
            return "silver", match
    return "grey", None


def build_halo(entries: list[WikiEntry], zot: ZoteroIndex, oa: OpenAlex) -> dict:
    wiki_by_doi: dict[str, WikiEntry] = {e.doi: e for e in entries if e.doi}
    nodes: dict[str, Node] = {}
    edges: list[Edge] = []
    stats = {
        "wiki_total": len(entries),
        "wiki_with_doi": sum(1 for e in entries if e.doi),
        "wiki_unscannable": 0,
        "openalex_404": 0,
        "openalex_resolved": 0,
        "neighbors_total": 0,
        "neighbors_gold": 0,
        "neighbors_silver": 0,
        "neighbors_grey": 0,
        "out_edges": 0,
        "in_edges": 0,
    }
    unscannable: list[str] = []
    not_in_openalex: list[str] = []

    def upsert_node(work: dict, default_color: str | None = None) -> Node | None:
        oa_id = _oa_id_of(work)
        doi = _doi_of(work)
        title = work.get("display_name") or work.get("title") or "(untitled)"
        key = oa_id or doi or title
        if key in nodes:
            return nodes[key]
        color, citekey = classify(work, wiki_by_doi, zot)
        if default_color:
            color = default_color
        node = Node(
            oa_id=oa_id,
            doi=doi,
            title=title,
            color=color,
            citekey=citekey,
            year=work.get("publication_year"),
            cited_by_count=work.get("cited_by_count"),
        )
        nodes[key] = node
        return node

    for e in entries:
        if not e.doi and not e.openalex_id:
            stats["wiki_unscannable"] += 1
            unscannable.append(e.file_path.name)
            continue
        work = (
            oa.work_by_id(e.openalex_id) if e.openalex_id else oa.work_by_doi(e.doi or "")
        )
        if not work:
            stats["openalex_404"] += 1
            not_in_openalex.append(e.file_path.name)
            # Still add the wiki entry as a gold node so the viz shows it.
            nodes[e.doi or e.file_path.stem] = Node(
                oa_id=None,
                doi=e.doi,
                title=e.title or e.file_path.stem,
                color="gold",
                citekey=e.citekey or e.item_key,
                year=None,
                cited_by_count=None,
            )
            continue
        stats["openalex_resolved"] += 1
        center = upsert_node(work, default_color="gold")
        if center is None:
            continue
        # Force gold (even if title-fuzzy missed) — the wiki entry is operator-owned.
        center.color = "gold"
        center.citekey = e.citekey or e.item_key

        # Out-edges: referenced_works (works this paper cites).
        ref_ids = list(work.get("referenced_works") or [])
        # Batch-fetch by id filter (50 per call max).
        for chunk in _chunk([rid.split("/")[-1] for rid in ref_ids if "/W" in rid], 50):
            data = oa.works_filter(
                filter_str=f"openalex:{'|'.join(chunk)}", per_page=len(chunk)
            )
            if not data or not data.get("results"):
                continue
            for w in data["results"]:
                n = upsert_node(w)
                if n:
                    edges.append(Edge(src=center.key(), dst=n.key(), kind="cites"))
                    stats["neighbors_total"] += 1
                    stats["out_edges"] += 1
                    stats[f"neighbors_{n.color}"] += 1

        # In-edges: top-K cited_by, sorted by their own cited_by_count desc.
        center_oa_id = _oa_id_of(work)
        if center_oa_id:
            in_data = oa.works_filter(
                filter_str=f"cites:{center_oa_id}",
                per_page=TOP_K_CITED_BY,
                sort="cited_by_count:desc",
            )
            if in_data and in_data.get("results"):
                for w in in_data["results"]:
                    n = upsert_node(w)
                    if n:
                        edges.append(Edge(src=n.key(), dst=center.key(), kind="cites"))
                        stats["neighbors_total"] += 1
                        stats["in_edges"] += 1
                        stats[f"neighbors_{n.color}"] += 1

    return {
        "nodes": nodes,
        "edges": edges,
        "stats": stats,
        "unscannable": unscannable,
        "not_in_openalex": not_in_openalex,
    }


def _chunk(seq: list[str], size: int) -> Iterable[list[str]]:
    for i in range(0, len(seq), size):
        yield seq[i : i + size]


# ──────────────────────────────────────────────────────────────────────────
# Output writers
# ──────────────────────────────────────────────────────────────────────────


def write_outputs(halo: dict, out_dir: Path, started_at: float) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)

    # NDJSON nodes & edges
    with (out_dir / "nodes.ndjson").open("w", encoding="utf-8") as f:
        for node in halo["nodes"].values():
            f.write(json.dumps(asdict(node), ensure_ascii=False) + "\n")
    with (out_dir / "edges.ndjson").open("w", encoding="utf-8") as f:
        for edge in halo["edges"]:
            f.write(json.dumps(asdict(edge), ensure_ascii=False) + "\n")

    # Markdown summary
    s = halo["stats"]
    nodes = list(halo["nodes"].values())
    n_gold = sum(1 for n in nodes if n.color == "gold")
    n_silver = sum(1 for n in nodes if n.color == "silver")
    n_grey = sum(1 for n in nodes if n.color == "grey")

    lines = [
        "---",
        "title: Wiki-halo — operator-owned citation graph (snapshot 2026-05-09)",
        "kind: zotero-coverage-report",
        "owner: cosmon",
        "tags: [wiki-halo, openalex, zotero-coverage, jr-c5]",
        "---",
        "",
        "# Wiki-halo — bridge diagnostic→poétique",
        "",
        "**Source artefacts.** ADR-091 §D9 · delib-20260509-39ad (jr verdict §1.5) · task-20260509-ca0a.",
        "",
        "**Tattoo.** *« A halo of the unknown. Gold for what is owned. Silver for what is",
        "known but not yet promoted. Grey for the un-touched neighbourhood. »*",
        "",
        "## Counts",
        "",
        "| Color   | Meaning                                 | Count |",
        "|---------|-----------------------------------------|-------|",
        f"| 🟡 gold   | in `wiki/`                              | {n_gold} |",
        f"| ⚪ silver | in Zotero, not yet in `wiki/`           | {n_silver} |",
        f"| ⚫ grey   | neither in `wiki/` nor in Zotero        | {n_grey} |",
        f"| **total**   |                                       | **{len(nodes)}** |",
        "",
        "## OpenAlex traversal",
        "",
        f"- wiki entries: **{s['wiki_total']}** (with DOI: {s['wiki_with_doi']}, unscannable: {s['wiki_unscannable']})",
        f"- OpenAlex resolved: **{s['openalex_resolved']}**, missing: {s['openalex_404']}",
        f"- out-edges (this cites …): **{s['out_edges']}**",
        f"- in-edges (… cites this, top {TOP_K_CITED_BY} per paper): **{s['in_edges']}**",
        f"- elapsed: **{time.monotonic() - started_at:.1f}s**",
        "",
        "## Per-citekey detail",
        "",
        "| wiki entry | resolved | out-deg | in-deg | gold-deg | silver-deg | grey-deg |",
        "|------------|----------|---------|--------|----------|------------|----------|",
    ]

    # Per-entry row from edges
    by_center: dict[str, dict[str, int]] = {}
    for n in nodes:
        if n.color == "gold":
            by_center[n.key()] = {"out": 0, "in": 0, "gold": 0, "silver": 0, "grey": 0}
    for e in halo["edges"]:
        # If src is a gold center → out-edge
        if e.src in by_center:
            by_center[e.src]["out"] += 1
            dst_color = halo["nodes"][e.dst].color if e.dst in halo["nodes"] else "grey"
            by_center[e.src][dst_color] += 1
        if e.dst in by_center:
            by_center[e.dst]["in"] += 1
            src_color = halo["nodes"][e.src].color if e.src in halo["nodes"] else "grey"
            by_center[e.dst][src_color] += 1

    for key, counts in sorted(by_center.items()):
        node = halo["nodes"][key]
        ck = node.citekey or "(no-citekey)"
        if ck and ck.lower() == "null":
            ck = "(no-citekey)"
        title_short = (node.title or "")[:60]
        lines.append(
            f"| `{ck}` — {title_short} | ✓ | {counts['out']} | {counts['in']} | "
            f"{counts['gold']} | {counts['silver']} | {counts['grey']} |"
        )

    if halo["unscannable"]:
        lines += ["", "## Unscannable (no DOI, no OpenAlex id)", ""]
        for f in halo["unscannable"]:
            lines.append(f"- `{f}`")
    if halo["not_in_openalex"]:
        lines += ["", "## Not in OpenAlex (404)", ""]
        for f in halo["not_in_openalex"]:
            lines.append(f"- `{f}`")

    lines += [
        "",
        "## Reading guide",
        "",
        "- `nodes.ndjson` — one JSON node per line: `{type,doi,oa_id,citekey,color,title}`",
        "- `edges.ndjson` — one JSON edge per line: `{src,dst,kind:cites}` (src cites dst)",
        "- `wiki-halo.canvas` — drop into Obsidian to render the colored halo (drag & drop, or `obsidian-cli open …`).",
        "",
        "## Phase 2 (not in scope today)",
        "",
        "1. Promote silver→gold by triaging the silver list (those Zotero items already exist; it's a wiki-page authoring decision).",
        "2. Investigate grey clusters — the densest grey neighbourhoods are the highest-value Zotero acquisition leads.",
        "3. Re-run weekly: a delta view shows what just appeared in the halo (alpha decay of the citation neighbourhood).",
        "",
    ]

    (out_dir / "wiki-halo.md").write_text("\n".join(lines), encoding="utf-8")

    # Obsidian Canvas (best-effort: gold centres only — full graph is too dense for a flat canvas)
    write_canvas(halo, out_dir / "wiki-halo.canvas")

    # run-stats
    (out_dir / "run-stats.json").write_text(
        json.dumps(
            {
                **s,
                "elapsed_seconds": round(time.monotonic() - started_at, 2),
                "node_color_counts": {"gold": n_gold, "silver": n_silver, "grey": n_grey},
            },
            indent=2,
        ),
        encoding="utf-8",
    )


def write_canvas(halo: dict, path: Path) -> None:
    """Lay out gold centres in a grid; cluster their immediate halo around them.

    For density reasons we only render edges to a node's *first-neighbour set*
    (gold centres + their direct silver/grey neighbours); deeper traversal would
    explode the canvas. This is a reading aid, not a graph database.
    """
    nodes_canvas = []
    edges_canvas = []
    seen: set[str] = set()

    gold = [n for n in halo["nodes"].values() if n.color == "gold"]
    gold.sort(key=lambda n: -(n.cited_by_count or 0))

    cols = 6
    cell_w, cell_h = 700, 480
    color_map = {"gold": "5", "silver": "0", "grey": "7"}  # Obsidian Canvas color id

    # Place each gold centre + its immediate halo (first-neighbour ring).
    centre_positions: dict[str, tuple[int, int]] = {}
    for i, n in enumerate(gold):
        cx = (i % cols) * cell_w
        cy = (i // cols) * cell_h
        centre_positions[n.key()] = (cx, cy)
        if n.key() not in seen:
            nodes_canvas.append(_canvas_node(n, cx, cy, color_map))
            seen.add(n.key())

    # Halo nodes: 8 around each centre, max — to keep canvas readable.
    import math
    for centre in gold:
        cx, cy = centre_positions[centre.key()]
        # Find this centre's neighbours via edges.
        nbrs: list[str] = []
        for e in halo["edges"]:
            if e.src == centre.key() and e.dst in halo["nodes"]:
                nbrs.append(e.dst)
            elif e.dst == centre.key() and e.src in halo["nodes"]:
                nbrs.append(e.src)
        # Dedup, drop other gold centres (already placed), cap at 8 per centre.
        seen_nbr = set()
        unique_nbrs: list[str] = []
        for nk in nbrs:
            if nk in seen_nbr:
                continue
            if halo["nodes"][nk].color == "gold":
                continue
            seen_nbr.add(nk)
            unique_nbrs.append(nk)
            if len(unique_nbrs) >= 8:
                break
        for j, nk in enumerate(unique_nbrs):
            angle = 2 * math.pi * j / max(1, len(unique_nbrs))
            ring_r = 200
            nx = int(cx + ring_r * math.cos(angle)) - 60
            ny = int(cy + ring_r * math.sin(angle)) + 100
            if nk not in seen:
                nodes_canvas.append(_canvas_node(halo["nodes"][nk], nx, ny, color_map))
                seen.add(nk)

    # Edges between rendered nodes only.
    rendered = {n["id"] for n in nodes_canvas}
    for e in halo["edges"]:
        sid, did = _edge_id(e.src), _edge_id(e.dst)
        if sid in rendered and did in rendered:
            edges_canvas.append({
                "id": f"{sid}->{did}",
                "fromNode": sid,
                "fromSide": "right",
                "toNode": did,
                "toSide": "left",
            })

    canvas = {"nodes": nodes_canvas, "edges": edges_canvas}
    path.write_text(json.dumps(canvas, indent=2, ensure_ascii=False), encoding="utf-8")


def _edge_id(key: str) -> str:
    return re.sub(r"[^A-Za-z0-9_-]", "_", key)[:64] or "n"


def _canvas_node(node: Node, x: int, y: int, color_map: dict[str, str]) -> dict:
    label_parts = []
    if node.citekey:
        label_parts.append(f"`{node.citekey}`")
    label_parts.append(f"**{node.title}**")
    if node.year:
        label_parts.append(f"({node.year})")
    if node.cited_by_count is not None:
        label_parts.append(f"cited: {node.cited_by_count}")
    if node.doi:
        label_parts.append(f"doi: {node.doi}")
    text = "\n".join(label_parts)
    return {
        "id": _edge_id(node.key()),
        "type": "text",
        "text": text,
        "x": x,
        "y": y,
        "width": 320,
        "height": 180,
        "color": color_map.get(node.color, "0"),
    }


# ──────────────────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────────────────


def main(argv: list[str]) -> int:
    started_at = time.monotonic()
    smoke = "--smoke" in argv
    limit_arg = next((a for a in argv if a.startswith("--limit=")), None)
    limit = int(limit_arg.split("=", 1)[1]) if limit_arg else None

    print(f"[wiki-halo] reading {WIKI_DIR}")
    entries = load_wiki_entries()
    print(f"[wiki-halo] {len(entries)} entries; {sum(1 for e in entries if e.doi)} have a DOI")

    if smoke:
        # Pick 3 entries that have DOIs for smoke testing.
        entries = [e for e in entries if e.doi][:3]
        print(f"[wiki-halo] SMOKE mode — limiting to {len(entries)} entries")
    elif limit is not None:
        entries = entries[:limit]
        print(f"[wiki-halo] limit={limit}")

    print("[wiki-halo] loading Zotero index from sqlite")
    zot = load_zotero_index()
    print(f"[wiki-halo] zotero: {len(zot.dois)} DOIs, {len(zot.titles)} titled items")

    print("[wiki-halo] querying OpenAlex (polite pool)")
    oa = OpenAlex()
    halo = build_halo(entries, zot, oa)

    write_outputs(halo, OUT_DIR, started_at)
    print(f"[wiki-halo] wrote outputs to {OUT_DIR}")
    print(f"[wiki-halo] stats: {halo['stats']}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
