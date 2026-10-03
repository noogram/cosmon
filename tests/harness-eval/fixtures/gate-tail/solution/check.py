import json, pathlib, sys

cfg = json.loads(pathlib.Path("config.json").read_text())
line = pathlib.Path("report.txt").read_text().strip()
sys.exit(0 if line == f"{cfg['name']} retries={cfg['retries']}" else 1)
