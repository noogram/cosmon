#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Independent acceptance for `large-context`: answer.txt names entry-0170 (value 9997)."""
import json, pathlib, sys

ws = pathlib.Path(sys.argv[1])
answer = ws / "answer.txt"
present = answer.is_file()
ok = present and answer.read_text().strip() == "entry-0170"
print(json.dumps({"artifacts": {"answer.txt": present}, "gate": None, "passed": ok}))
sys.exit(0 if ok else 1)
