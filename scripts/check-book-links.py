#!/usr/bin/env python3
"""Fail closed on broken local mdBook links and report external link concerns.

The book must be reviewable without network access: every relative target and
fragment is therefore validated from the checked-out source.  HTTP(S) targets
are deliberately advisory because remote availability is not a reproducible
property of a pull request; a dead target or a specific URL redirected to a
site home is still reported prominently for the reviewer.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import html
import re
import sys
import unicodedata
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter
from dataclasses import dataclass
from pathlib import Path


LINK = re.compile(r"!?\[[^\]]*\]\((?:<([^>]+)>|([^\s)]+))(?:\s+[^)]*)?\)")
REFERENCE = re.compile(r"^\s*\[([^\]]+)\]:\s*(?:<([^>]+)>|(\S+))", re.MULTILINE)
HTML_HREF = re.compile(
    r"<[A-Za-z][^>]*?\bhref\s*=\s*(?:\"([^\"]*)\"|'([^']*)'|([^\s\"'=<>`]+))[^>]*>"
)
HEADING = re.compile(r"^#{1,6}\s+(.*?)(?:\s+#+)?\s*$")
MARKUP = re.compile(r"[`*_~]|<[^>]*>|\[[^]]*\]\([^)]*\)")


@dataclass(frozen=True)
class Link:
    source: Path
    line: int
    target: str


def markdown_files(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*.md") if path.is_file())


def strip_fenced_code(text: str) -> str:
    lines: list[str] = []
    fence: tuple[str, int] | None = None
    for line in text.splitlines(keepends=True):
        match = re.match(r"^ {0,3}(`+|~+)", line)
        if fence is None:
            if match is None or len(match.group(1)) < 3:
                lines.append(line)
                continue
            marker = match.group(1)
            fence = (marker[0], len(marker))
            lines.append("\n")
            continue

        lines.append("\n")
        if match is None:
            continue
        marker = match.group(1)
        if marker[0] == fence[0] and len(marker) >= fence[1]:
            fence = None
    return "".join(lines)


def links_in(path: Path) -> list[Link]:
    text = strip_fenced_code(path.read_text(encoding="utf-8"))
    links: list[Link] = []
    for match in LINK.finditer(text):
        target = match.group(1) or match.group(2)
        links.append(Link(path, text.count("\n", 0, match.start()) + 1, target))
    for match in REFERENCE.finditer(text):
        target = match.group(2) or match.group(3)
        links.append(Link(path, text.count("\n", 0, match.start()) + 1, target))
    for match in HTML_HREF.finditer(text):
        target = html.unescape(match.group(1) or match.group(2) or match.group(3))
        links.append(Link(path, text.count("\n", 0, match.start()) + 1, target))
    return links


def anchor_slug(text: str) -> str:
    text = MARKUP.sub("", text).strip().lower()
    text = "".join(char for char in unicodedata.normalize("NFKD", text) if not unicodedata.combining(char))
    text = re.sub(r"[^\w\s-]", "", text, flags=re.UNICODE)
    return re.sub(r"[\s-]+", "-", text).strip("-")


def anchors_in(path: Path) -> set[str]:
    counts: Counter[str] = Counter()
    anchors: set[str] = set()
    for line in strip_fenced_code(path.read_text(encoding="utf-8")).splitlines():
        match = HEADING.match(line)
        if match is None:
            continue
        slug = anchor_slug(match.group(1))
        if not slug:
            continue
        count = counts[slug]
        anchors.add(slug if count == 0 else f"{slug}-{count}")
        counts[slug] += 1
    return anchors


def local_target(root: Path, link: Link) -> tuple[Path | None, str | None]:
    target = urllib.parse.unquote(link.target)
    # Generated command reference pages retain Rust intra-doc identifiers as
    # prose links.  `crate::module::Item` is not an mdBook-relative path.
    if "::" in target:
        return None, None
    parts = urllib.parse.urlsplit(target)
    if parts.scheme or parts.netloc or target.startswith("/"):
        return None, None
    target_path = urllib.parse.unquote(parts.path)
    resolved = link.source.parent / target_path if target_path else link.source
    return resolved.resolve(), urllib.parse.unquote(parts.fragment) or None


def external_warning(url: str) -> str | None:
    request = urllib.request.Request(url, headers={"User-Agent": "cosmon-book-linkcheck/1"})
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            final = response.geturl()
            status = response.status
    except urllib.error.HTTPError as error:
        return f"HTTP {error.code}"
    except (urllib.error.URLError, TimeoutError, ValueError) as error:
        return f"unreachable ({error})"

    original = urllib.parse.urlsplit(url)
    destination = urllib.parse.urlsplit(final)
    specific = original.path not in ("", "/") or bool(original.query) or bool(original.fragment)
    home = destination.path in ("", "/") and not destination.query and not destination.fragment
    if status >= 400:
        return f"HTTP {status}"
    if specific and home and original.netloc == destination.netloc:
        return f"redirected to site home ({final})"
    return None


def display(root: Path, link: Link) -> str:
    return f"{link.source.relative_to(root)}:{link.line} -> {link.target}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=Path("docs/book/src"))
    parser.add_argument("--offline", action="store_true", help="skip advisory HTTP(S) probes")
    args = parser.parse_args()
    root = args.root.resolve()
    if not root.is_dir():
        print(f"check-book-links: book source does not exist: {root}", file=sys.stderr)
        return 2

    local_failures: list[str] = []
    external: dict[str, list[Link]] = {}
    anchors: dict[Path, set[str]] = {}
    for file in markdown_files(root):
        for link in links_in(file):
            if link.target.startswith(("http://", "https://")):
                external.setdefault(link.target, []).append(link)
                continue
            resolved, fragment = local_target(root, link)
            if resolved is None:
                continue
            if not resolved.is_file():
                local_failures.append(f"missing target: {display(root, link)}")
                continue
            if fragment:
                available = anchors.setdefault(resolved, anchors_in(resolved))
                if fragment not in available:
                    local_failures.append(f"missing anchor #{fragment}: {display(root, link)}")

    for failure in local_failures:
        print(f"ERROR: {failure}", file=sys.stderr)

    if not args.offline:
        with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
            results = pool.map(external_warning, external)
            for url, warning in zip(external, results, strict=True):
                if warning is None:
                    continue
                for link in external[url]:
                    print(f"WARNING: external {warning}: {display(root, link)}", file=sys.stderr)

    if local_failures:
        print(f"check-book-links: FAIL — {len(local_failures)} broken local link(s).", file=sys.stderr)
        return 1
    print("check-book-links: PASS — local targets and anchors resolve.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
