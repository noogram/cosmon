#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""license-network-boundary.py — enforce the network-boundary doctrine.

Doctrine (delib-20260620-ca76, codified in the root LICENSE + ADR-092):

    A first-party crate may stay Apache-2.0 IFF its real `[dependencies]`
    closure (normal edges only — NOT dev, NOT build) contains zero
    AGPL-3.0-only first-party crates. I.e. an Apache crate may reach the
    AGPL core only over the network (HTTP/IPC), never by code-linking.

Any Apache-2.0 first-party crate that transitively code-links an
AGPL-3.0-only first-party crate through normal dependencies is a
license contradiction and must flip to AGPL. This script is the CI gate
that catches that drift before it ships.

It deliberately ignores dev- and build-dependencies: those are the false
positives the flat layer-map produced (delib-ca76 §d.5). Linking an AGPL
crate as a `[dev-dependencies]` test helper does not encumber the shipped
artifact.

Usage:
    python3 scripts/license-network-boundary.py            # CI gate
    python3 scripts/license-network-boundary.py --verbose  # show closures

Exit codes: 0 = clean, 1 = contradiction found, 2 = tooling error.
"""
import json
import subprocess
import sys

AGPL = "AGPL-3.0-only"
APACHE = "Apache-2.0"


def cargo_metadata():
    try:
        out = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--all-features"],
            capture_output=True, text=True, check=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        print(f"network-boundary: cargo metadata failed: {e}", file=sys.stderr)
        sys.exit(2)
    return json.loads(out)


def main():
    verbose = "--verbose" in sys.argv
    md = cargo_metadata()

    workspace = set(md["workspace_members"])
    # id -> (name, license)
    pkg = {}
    for p in md["packages"]:
        pkg[p["id"]] = (p["name"], p.get("license") or "")

    first_party = {pid for pid in workspace}

    # Build the normal-dependency graph over FIRST-PARTY crates only.
    # resolve.nodes[].deps[].dep_kinds[].kind: null=normal, "dev", "build".
    normal_edges = {pid: set() for pid in first_party}
    for node in md["resolve"]["nodes"]:
        src = node["id"]
        if src not in first_party:
            continue
        for dep in node["deps"]:
            tgt = dep["pkg"]
            if tgt not in first_party:
                continue  # third-party (crates.io) — handled by cargo-deny
            kinds = dep.get("dep_kinds") or []
            # A normal edge has at least one dep_kind whose "kind" is null.
            if any(k.get("kind") is None for k in kinds):
                normal_edges[src].add(tgt)

    def closure(root):
        seen, stack = set(), [root]
        while stack:
            cur = stack.pop()
            for nxt in normal_edges.get(cur, ()):
                if nxt not in seen:
                    seen.add(nxt)
                    stack.append(nxt)
        return seen

    violations = []
    apache_crates = sorted(
        pid for pid in first_party if pkg[pid][1] == APACHE
    )
    for pid in apache_crates:
        name, _ = pkg[pid]
        agpl_links = sorted(
            pkg[d][0] for d in closure(pid) if pkg[d][1] == AGPL
        )
        if verbose:
            cl = sorted(pkg[d][0] for d in closure(pid))
            print(f"  {name} (Apache) normal-closure: {cl or '(none)'}")
        if agpl_links:
            violations.append((name, agpl_links))

    print(
        f"network-boundary: checked {len(apache_crates)} Apache-2.0 "
        f"first-party crate(s) against the normal-dependency graph."
    )
    if not violations:
        print("network-boundary: OK — no Apache crate code-links an AGPL crate.")
        return 0

    print("\nnetwork-boundary: CONTRADICTION(S) FOUND:", file=sys.stderr)
    for name, links in violations:
        print(
            f"  - Apache crate '{name}' code-links AGPL crate(s) via normal "
            f"deps: {', '.join(links)}", file=sys.stderr,
        )
    print(
        "\nResolve: flip the Apache crate to AGPL-3.0-only, OR sever the link "
        "so it reaches the core only over the network (HTTP/IPC).",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
