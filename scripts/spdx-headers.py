#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""spdx-headers.py — stamp every first-party `.rs` file with an SPDX header.

The header is `// SPDX-License-Identifier: <LICENSE>` as line 1, where
<LICENSE> is the effective license of the crate the file belongs to
(resolved from that crate's Cargo.toml `license` field, falling back to
the workspace default AGPL-3.0-only). This is the reproducible tool behind
the per-file headers mandated by the open-core re-license
(delib-20260620-ca76); committing it keeps the 1000+ headers from drifting.

Idempotent: a file that already carries the correct header is skipped; a
file with a WRONG header has line 1 rewritten in place; a file with no
header gets one prepended (+ a blank separator line).

Two classes of `.rs` are EXCLUDED — stamping them would be wrong, not just
unnecessary:

  1. Vendored third-party trees (`**/vendor/**`). Their upstream license is
     preserved verbatim; we never overwrite someone else's header. (See
     NOTICE for the enumeration: matrix-sdk, llama.cpp, …)
  2. `trybuild` compile-fail fixtures (`**/tests/compile_fail/**`). Their
     line/column numbers are load-bearing — they are matched against golden
     `.stderr` files. Prepending a header shifts every line by two and
     breaks the goldens. These are test inputs, not shipped source.

Usage:
    python3 scripts/spdx-headers.py          # stamp / fix in place
    python3 scripts/spdx-headers.py --check   # CI gate: exit 1 if any file
                                              # lacks the correct header
"""
import re
import sys
from pathlib import Path

WS_DEFAULT = "AGPL-3.0-only"
EXCLUDE_SUBSTRINGS = ("/vendor/", "/target/", "/tests/compile_fail/")


def crate_license(cargo: Path) -> str:
    txt = cargo.read_text()
    m = re.search(r'^license\s*=\s*"([^"]+)"', txt, re.M)
    if m:
        return m.group(1)
    return WS_DEFAULT  # `license.workspace = true` or absent → workspace default


def excluded(path: Path) -> bool:
    s = str(path)
    return any(sub in s for sub in EXCLUDE_SUBSTRINGS)


def iter_crates(root: Path):
    yield root / "xtask"
    crates = root / "crates"
    if crates.is_dir():
        for d in sorted(crates.iterdir()):
            yield d


def main() -> int:
    check = "--check" in sys.argv
    root = Path(__file__).resolve().parent.parent
    new = fixed = ok = missing = 0
    for crate in iter_crates(root):
        cargo = crate / "Cargo.toml"
        if not cargo.exists():
            continue
        header = f"// SPDX-License-Identifier: {crate_license(cargo)}"
        for rs in crate.rglob("*.rs"):
            if excluded(rs):
                continue
            lines = rs.read_text().splitlines(keepends=True)
            first = lines[0].rstrip("\n") if lines else ""
            if first.startswith("// SPDX-License-Identifier:"):
                if first.strip() == header:
                    ok += 1
                    continue
                if check:
                    missing += 1
                    print(f"  wrong header: {rs}", file=sys.stderr)
                    continue
                lines[0] = header + "\n"
                rs.write_text("".join(lines))
                fixed += 1
                continue
            # No header.
            if check:
                missing += 1
                print(f"  missing header: {rs}", file=sys.stderr)
                continue
            body = "".join(lines)
            sep = "\n" if (lines and not lines[0].startswith("\n")) else ""
            rs.write_text(header + "\n" + sep + body)
            new += 1

    if check:
        if missing:
            print(
                f"spdx-headers: {missing} file(s) lack a correct SPDX header. "
                f"Run: python3 scripts/spdx-headers.py", file=sys.stderr,
            )
            return 1
        print(f"spdx-headers: OK — {ok} first-party .rs files carry a header.")
        return 0

    print(f"spdx-headers: new={new} fixed={fixed} already-ok={ok}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
