#!/usr/bin/env python3
"""Part XVIII empirical validation: compute eta_coupling per molecule.

eta_coupling := I(next_action; metric) / tokens_to_project
Feynman four-criterion: honest, falsifiable, reproducible, <50 lines.
"""
import json, math, pathlib, sys
from collections import Counter, defaultdict
STATE = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".cosmon/state")
LOG = STATE / "log" / "energy.jsonl"
MOLS = STATE / "fleets" / "default" / "molecules"
def mi(pairs):  # empirical Shannon MI, bits
    if not pairs: return 0.0
    n = len(pairs); joint = Counter(pairs)
    mx = Counter(a for a, _ in pairs); my = Counter(b for _, b in pairs)
    return sum((c/n) * math.log2((c/n)/((mx[a]/n)*(my[b]/n)))
               for (a,b), c in joint.items() if c)
agg = defaultdict(lambda: {"in": 0, "out": 0, "cost": 0.0})
if LOG.exists():
    for line in LOG.read_text().splitlines():
        try: r = json.loads(line)
        except json.JSONDecodeError: continue
        m = agg[r["molecule"]]
        m["in"] += r.get("input_tokens", 0)
        m["out"] += r.get("output_tokens", 0)
        m["cost"] += r.get("cost", 0.0)
pairs, rows = [], []
for d in sorted(MOLS.glob("*")) if MOLS.exists() else []:
    try: st = json.loads((d/"state.json").read_text())
    except (FileNotFoundError, json.JSONDecodeError): continue
    mol = d.name; e = agg.get(mol, {"in":0,"out":0,"cost":0.0})
    step = st.get("current_step", 0)
    bucket = int(math.log2(1 + e["cost"] * 1e4))
    pairs.append((step, bucket))
    rows.append((mol, step, e["in"]+e["out"], e["cost"]))
total = mi(pairs)
print(f"{'molecule':<28} {'step':>4} {'tokens':>10} {'cost_usd':>10} {'eta_coupling':>14}")
for mol, step, tok, cost in rows:
    eta = (total / tok) if tok else 0.0
    print(f"{mol:<28} {step:>4} {tok:>10} {cost:>10.4f} {eta:>14.6e}")
print(f"\nI(next_action; cost_bucket) = {total:.4f} bits over {len(pairs)} molecules")
