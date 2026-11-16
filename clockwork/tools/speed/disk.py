"""What a recorded trace keeps on disk: the trace, its pictures (distinct, in the cache), a snapshot's size.

usage: disk.py TRACE CACHE_OBJECTS LOG SECONDS
"""
import json
import os
import re
import sys

trace, objects, log, seconds = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
sha = re.compile(r"^[0-9a-f]{64}$")
refreshes, pictures, screens = 0, set(), set()
for n, line in enumerate(open(trace, encoding="utf-8")):
    if n == 0:
        continue
    t = json.loads(line)
    for f in t.get("frames", []) if isinstance(t, dict) else []:
        refreshes += 1
        for v in f:
            if isinstance(v, str) and sha.match(v):
                pictures.add(v)
    s = t.get("screen") if isinstance(t, dict) else None
    if isinstance(s, str) and sha.match(s):
        screens.add(s)


def size(h):
    p = os.path.join(objects, h[:2], h[2:])
    return os.path.getsize(p) if os.path.exists(p) else 0


pic_bytes = sum(size(h) for h in pictures)
scr_bytes = sum(size(h) for h in screens - pictures)
snap = None
for line in open(log, encoding="utf-8"):
    r = json.loads(line)["request"]
    if r["op"] == "load_snapshot":
        snap = os.path.getsize(r["file"])
saves = sum(1 for line in open(log, encoding="utf-8") if json.loads(line)["request"]["op"] == "save")
hour = 3600 / seconds
mb = 2**20
print(f"trace     {os.path.getsize(trace) / mb:.1f} MB")
print(f"refreshes {refreshes}, {len(pictures)} distinct pictures kept, {pic_bytes / mb:.1f} MB "
      f"({pic_bytes / max(len(pictures), 1) / 1024:.1f} KB each)")
print(f"screens   {len(screens)} distinct at ticks ({len(screens - pictures)} not among the pictures, {scr_bytes / mb:.1f} MB)")
print(f"snapshot  {snap / mb:.2f} MB each, {saves} taken")
total = os.path.getsize(trace) + pic_bytes + scr_bytes + saves * snap
print(f"total     {total / mb:.1f} MB for {seconds:.0f} s: {total * hour / mb:.0f} MB an hour "
      f"(pictures {pic_bytes * hour / mb:.0f}, snapshots {saves * snap * hour / mb:.0f}, trace {os.path.getsize(trace) * hour / mb:.0f})")
