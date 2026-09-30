#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Keep the remote harvest and binding documentation aligned with the code."""

from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def main() -> None:
    adr = read("docs/adr/176-remote-harvest-authority-is-a-sealed-capability.md")
    spec = read("docs/specs/CosmonRun.tla")
    openapi = read("crates/cosmon-rpp-adapter/openapi/v1.yaml")
    design = read("crates/cosmon-rpp-adapter/docs/admin-provisioning-design.md")
    checks = {
        "ADR scope source": "Where\n   the scope may be sourced from is open" not in adr
        and "The dedicated harvest scope is binding-only" in adr,
        "ADR implementation status": "**not implemented**" not in adr
        and "**Status:** adopted as contract; W4–W7 implemented" in adr,
        "purge model status": "implementation does NOT honour this" not in spec
        and "issue #122, open" not in spec
        and "implementation preserves the Running molecule" in spec,
        "published rejection reasons": "seal_broken" not in openapi,
        "binding validation design": "seal_intact" not in design
        and "Le hash retourné décrit le fichier rendu" in design,
    }
    failures = [name for name, passed in checks.items() if not passed]
    if failures:
        raise SystemExit("stale remote harvest documentation: " + ", ".join(failures))


if __name__ == "__main__":
    main()
