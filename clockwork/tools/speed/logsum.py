"""Sums a protocol log (RRC_PROTOCOL_LOG) by op: count, total ms, answer bytes."""
import json
import sys
from collections import defaultdict

ops = defaultdict(lambda: [0, 0.0, 0, 0])
for line in open(sys.argv[1], encoding="utf-8"):
    e = json.loads(line)
    op = e["request"]["op"]
    o = ops[op]
    o[0] += 1
    o[1] += e["ms"]
    o[2] += e["answer"]
    o[3] = max(o[3], e["answer"])
total = sum(o[1] for o in ops.values())
for op, (n, ms, bytes_, biggest) in sorted(ops.items(), key=lambda kv: -kv[1][1]):
    print(f"{op:20} {n:6} asks {ms / 1000:8.1f} s  {bytes_ / 2**20:8.1f} MB answered (largest {biggest / 1024:.0f} KB)")
print(f"{'all':20} {sum(o[0] for o in ops.values()):6} asks {total / 1000:8.1f} s")
