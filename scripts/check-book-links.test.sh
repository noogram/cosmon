#!/usr/bin/env bash
# Hermetic regression tests for the offline half of check-book-links.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
gate="$here/check-book-links.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/book/guide"

cat > "$work/book/SUMMARY.md" <<'MD'
# Summary

[Guide](guide/page.md#valid-anchor)
MD
cat > "$work/book/guide/page.md" <<'MD'
# Valid anchor

[Local anchor](#valid-anchor)
[Rust item](cosmon_core::module::Item)
MD

"$gate" --offline "$work/book" >/dev/null
echo "PASS: valid relative target and anchors resolve"

printf '\n[missing](absent.md)\n' >> "$work/book/guide/page.md"
if "$gate" --offline "$work/book" >/dev/null 2>&1; then
  echo "FAIL: missing local target must fail" >&2
  exit 1
fi
echo "PASS: missing local target fails"
sed -i.bak '$d' "$work/book/guide/page.md"
rm "$work/book/guide/page.md.bak"

printf '\n[missing anchor](#not-here)\n' >> "$work/book/guide/page.md"
if "$gate" --offline "$work/book" >/dev/null 2>&1; then
  echo "FAIL: missing anchor must fail" >&2
  exit 1
fi
echo "PASS: missing local anchor fails"
sed -i.bak '$d' "$work/book/guide/page.md"
rm "$work/book/guide/page.md.bak"

cat > "$work/book/guide/fence.md" <<'MD'
````text
The shorter marker below is content, not a closing fence.
```
````

[missing after nested fence](absent-after-fence.md)
MD
if "$gate" --offline "$work/book" >/dev/null 2>&1; then
  echo "FAIL: a broken link after a nested fence must fail" >&2
  exit 1
fi
echo "PASS: nested fenced-code markers do not hide later links"
rm "$work/book/guide/fence.md"

cat > "$work/book/guide/raw-html.md" <<'MD'
<a href="absent-from-html.md">Missing HTML target</a>
MD
if "$gate" --offline "$work/book" >/dev/null 2>&1; then
  echo "FAIL: a broken raw HTML href must fail" >&2
  exit 1
fi
echo "PASS: raw HTML href targets are checked"
