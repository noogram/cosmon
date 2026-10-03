#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Independent acceptance for `text-output`: answer.txt holds the sum, 876."""
import json, pathlib, sys

ws = pathlib.Path(sys.argv[1])
answer = ws / "answer.txt"
present = answer.is_file()
ok = present and answer.read_text().strip() == "876"
print(json.dumps({"artifacts": {"answer.txt": present}, "gate": None, "passed": ok}))
sys.exit(0 if ok else 1)
