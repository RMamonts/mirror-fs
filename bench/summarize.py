#!/usr/bin/env python3
"""Prints the median over repetitions of every (scenario, variant, role)."""
import json, sys, statistics
from collections import defaultdict

rows = defaultdict(list)
for line in open(sys.argv[1]):
    r = json.loads(line)
    rows[(r["scenario"], r["role"], r["variant"])].append(r)

order = ["one:inline", "one:blocking", "one:sync", "two:inline", "two:sync", "two:sync:y"]
keys = ["ops_s", "mib_s", "p50_us", "p99_us", "p999_us", "max_us"]
last = None
for (scen, role, var) in sorted(rows, key=lambda k: (k[0], k[1], order.index(k[2]) if k[2] in order else 99)):
    if (scen, role) != last:
        print(f"\n## {scen} / {role}  (n={len(rows[(scen, role, var)])})")
        print(f"{'variant':<14}" + "".join(f"{k:>10}" for k in keys) + f"{'errors':>8}")
        last = (scen, role)
    rs = rows[(scen, role, var)]
    med = {k: statistics.median(r[k] for r in rs) for k in keys}
    spread = max(r["ops_s"] for r in rs) / max(1, min(r["ops_s"] for r in rs)) - 1
    print(f"{var:<14}" + "".join(f"{med[k]:>10.0f}" if k != "mib_s" else f"{med[k]:>10.1f}" for k in keys)
          + f"{sum(r['errors'] for r in rs):>8}   ops spread {spread*100:.0f}%")
