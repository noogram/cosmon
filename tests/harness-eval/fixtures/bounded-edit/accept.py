#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Independent acceptance for `bounded-edit`: external tests on textstat."""
import json, pathlib, subprocess, sys

ws = pathlib.Path(sys.argv[1])
code = (
    "from textstat.util import mean, median\n"
    "assert median([3, 1, 2]) == 2\n"
    "assert median([4, 1, 3, 2]) == 2.5\n"
    "assert median([7]) == 7\n"
    "assert median([1, 2]) == 1.5\n"
    "assert mean([1, 2, 3]) == 2\n"
)
done = subprocess.run([sys.executable, "-c", code], cwd=ws, capture_output=True)
ok = done.returncode == 0
print(json.dumps({"artifacts": {"textstat/util.py": (ws / "textstat/util.py").is_file()},
                  "gate": ok, "passed": ok}))
sys.exit(0 if ok else 1)
