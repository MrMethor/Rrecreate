"""Speed over a run's stretches, from a protocol log (rrc's, or replay.py's --ticks-out file): each tick request's
play (from its `from` to the next one's) against the host time it took.

usage: stretches.py FILE [--window SECONDS]
"""
import json
import sys

IPS = 8_000_000
window = float(sys.argv[sys.argv.index("--window") + 1]) if "--window" in sys.argv else 10.0
ticks = []
for line in open(sys.argv[1], encoding="utf-8"):
    e = json.loads(line)
    r = e.get("request", e)
    if r.get("op", "tick") == "tick":
        ticks.append((r["from"], e["ms"] / 1000))
spans = [(ticks[i][0], (ticks[i + 1][0] - ticks[i][0]) / IPS, ticks[i][1]) for i in range(len(ticks) - 1)]
play = sum(p for _, p, _ in spans)
host = sum(h for _, _, h in spans)
print(f"ticks     {len(spans)}: {play:.1f} s of play in {host:.1f} s, {play / host:.2f}x")
start = spans[0][0]
longest = sorted(spans, key=lambda s: -s[1])[:3]
for c, p, h in longest:
    print(f"long      a pass of {p:.1f} s of play at {(c - start) / IPS:.0f} s: {h:.1f} s, {p / h:.2f}x")
# The slowest stretch of about `window` seconds of play.
worst, i, j, acc_p, acc_h = None, 0, 0, 0.0, 0.0
while j < len(spans):
    acc_p += spans[j][1]
    acc_h += spans[j][2]
    j += 1
    while acc_p - spans[i][1] >= window and i < j - 1:
        acc_p -= spans[i][1]
        acc_h -= spans[i][2]
        i += 1
    if acc_p >= window and (worst is None or acc_p / acc_h < worst[0]):
        worst = (acc_p / acc_h, (spans[i][0] - start) / IPS, acc_p)
if worst:
    print(f"slowest   {worst[2]:.0f} s of play from {worst[1]:.0f} s: {worst[0]:.2f}x")
