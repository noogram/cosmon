#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Independent acceptance for `gate-tail`: artifacts, then the gate, from a pristine check."""
import json, pathlib, subprocess, sys

ws = pathlib.Path(sys.argv[1])
here = pathlib.Path(__file__).resolve().parent
artifacts = {n: (ws / n).is_file() for n in ("config.json", "report.txt")}
gate = False
if all(artifacts.values()):
    # Run the pristine gate script, not the (editable) copy in the workspace.
    done = subprocess.run([sys.executable, str(here / "input" / "check.py")], cwd=ws, capture_output=True)
    cfg_ok = False
    try:
        cfg = json.loads((ws / "config.json").read_text())
        cfg_ok = cfg.get("name") == "demo" and cfg.get("retries") == 3
    except ValueError:
        pass
    gate = done.returncode == 0 and cfg_ok
print(json.dumps({"artifacts": artifacts, "gate": gate, "passed": gate}))
sys.exit(0 if gate else 1)
