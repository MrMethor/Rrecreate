"""The window's emulation per frame, headless: a session's snapshot and input loaded in `clockwork serve`, then run a
70 Hz frame of cycles at a time (as `clockwork play` does), each frame's host time measured. Optionally with a
follower beside it: replay.py's full replay of a protocol log, at normal or below-normal priority.

usage: pacing.py CLOCKWORK LOG --frames N [--follower normal|below] [--hash-every N]
"""
import json
import statistics
import subprocess
import sys
import time

exe, log = sys.argv[1], sys.argv[2]
frames = int(sys.argv[sys.argv.index("--frames") + 1])
follower = sys.argv[sys.argv.index("--follower") + 1] if "--follower" in sys.argv else None
hash_every = int(sys.argv[sys.argv.index("--hash-every") + 1]) if "--hash-every" in sys.argv else 0
requests = [json.loads(line)["request"] for line in open(log, encoding="utf-8")]
IPS = 8_000_000
PER_FRAME = IPS // 70
BUDGET_MS = 1000 / 70

side = None
if follower:
    flags = 0x00004000 if follower == "below" else 0  # BELOW_NORMAL_PRIORITY_CLASS
    side = subprocess.Popen([sys.executable, __file__.replace("pacing.py", "replay.py"), exe, log, "--ticks", "100000",
                             "full"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=flags)
    time.sleep(3)  # its load and first ticks under way

child = subprocess.Popen([exe, "serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)


def ask(r):
    child.stdin.write((json.dumps(r) + "\n").encode())
    child.stdin.flush()
    a = json.loads(child.stdout.readline())
    if "error" in a:
        raise SystemExit(f"{r['op']}: {a['error']}")
    return a


for r in requests:
    if r["op"] in ("hello", "load_snapshot", "input"):
        ask(r)
if "--plain" in sys.argv:
    # The recording's snapshot carries its call tracking: off, as a window from power-on has none.
    ask({"op": "calls", "id": 0, "on": False})
times, hashes = [], []
for n in range(frames):
    t = time.perf_counter()
    ask({"op": "run", "id": n, "cycles": PER_FRAME})
    times.append((time.perf_counter() - t) * 1000)
    if hash_every and n % hash_every == hash_every - 1:
        t = time.perf_counter()
        ask({"op": "hash", "id": n})
        hashes.append((time.perf_counter() - t) * 1000)
child.stdin.close()
child.wait()
if side:
    side.kill()
q = statistics.quantiles(times, n=100)
late = sum(1 for t in times if t > BUDGET_MS)
print(f"frames    {frames} ({frames / 70:.0f} s of play), follower {follower or 'none'}")
print(f"per frame mean {statistics.mean(times):.2f} ms, median {q[49]:.2f}, p95 {q[94]:.2f}, p99 {q[98]:.2f}, "
      f"max {max(times):.2f} (budget {BUDGET_MS:.2f}); {late} over the budget ({100 * late / frames:.1f}%)")
if hashes:
    print(f"hash      {len(hashes)}: mean {statistics.mean(hashes):.1f} ms, max {max(hashes):.1f} ms")
