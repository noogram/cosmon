#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Independent acceptance for `interrupted-effects`: each effect exactly once, in the log."""
import json, pathlib, sys

ws = pathlib.Path(sys.argv[1])
log = ws / "effects.log"
present = log.is_file()
lines = log.read_text().splitlines() if present else []
ok = present and sorted(lines) == ["applied e1", "applied e2", "applied e3"]
print(json.dumps({"artifacts": {"effects.log": present}, "gate": None, "passed": ok}))
sys.exit(0 if ok else 1)
