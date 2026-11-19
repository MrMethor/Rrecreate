"""Replays a protocol log against `clockwork serve`, with what a recording tracks left out, and times it (M6.5's
step 0). The log is rrc's: `RRC_PROTOCOL_LOG=FILE rrc record SESSION --run` appends each request to FILE. A replay
is the recording's run again (Clockwork is deterministic, and the analysis tools never change what the machine does);
snapshots aren't saved, and the requests after the last tick are left out.

usage: replay.py CLOCKWORK LOG --ticks N [--ticks-out FILE] [--answers FILE] VARIANT...

variants:
  full        as recorded
  no-calls, no-images, no-late, no-hooks, no-data, no-screen, no-reads
              one thing left out: the call tracker's request, the pictures between ticks, the late reads' watch,
              the hooks' entries, the data segment kept at each tick (D92), the screen at each tick, every read
  bare        all of them left out (a recording's snapshot still brings its call tracker)
  plain       bare, and the call tracker turned off: emulation alone
  hash        as recorded, with a state hash after every tick
  run, plain-run
              no sync points: one run to where the last tick began
--ticks-out writes each tick's start cycle and host time, a JSON line each, for stretches.py.
Each variant's line has its wall time and Clockwork's own CPU time, which other programs running beside it move far
less (two builds compared on a machine in use, D210).
--answers writes each answer's request and a hash of what it holds, a line each: two builds that answered the same
          (a change meant to make Clockwork faster, D210) write the same file.
"""
import hashlib
import json
import subprocess
import sys
import time
from collections import defaultdict

exe, log = sys.argv[1], sys.argv[2]
ticks = int(sys.argv[sys.argv.index("--ticks") + 1])
out_path = sys.argv[sys.argv.index("--ticks-out") + 1] if "--ticks-out" in sys.argv else None
answers_path = sys.argv[sys.argv.index("--answers") + 1] if "--answers" in sys.argv else None
variants = [v for v in sys.argv[3:] if v not in ("--ticks", str(ticks), "--ticks-out", out_path, "--answers",
                                                  answers_path)]
ticks_out = open(out_path, "w", encoding="utf-8") if out_path else None
answers = open(answers_path, "w", encoding="utf-8") if answers_path else None
requests = [json.loads(line)["request"] for line in open(log, encoding="utf-8")]
LEFT_OUT = ("save", "entries_end", "late_inputs_end", "frames", "take_frame_images", "sound")
EVERYTHING = {"calls", "images", "late", "hooks", "data", "screen", "reads"}
FOR = {"calls": "calls", "frame_images": "images", "late_inputs": "late", "late_inputs_take": "late", "entries": "hooks",
       "entries_take": "hooks", "read": "reads"}


def keep(variant, r):
    """The request as the variant sends it, or None."""
    if r["op"] in LEFT_OUT:
        return None
    off = EVERYTHING if variant in ("bare", "plain") else {variant[3:]} if variant.startswith("no-") else set()
    if FOR.get(r["op"]) in off:
        return None
    if r["op"] == "tick":
        r = dict(r)
        if "data" in off:
            r["ranges"] = [x for x in r["ranges"] if x.get("len") != 0x10000]
        if "reads" in off:
            r["ranges"] = []
        if "screen" in off:
            r["screen"] = False
    return r


def cpu_seconds(child):
    """The CPU time `child` has used, user and kernel, while it is still running (Windows), or once it ended
    (elsewhere: `wait`s for it)."""
    if sys.platform == "win32":
        import ctypes
        import ctypes.wintypes as wt
        times = [wt.FILETIME() for _ in range(4)]
        ok = ctypes.WinDLL("kernel32").GetProcessTimes(wt.HANDLE(int(child._handle)), *map(ctypes.byref, times))
        if not ok:
            return float("nan")
        return sum((t.dwHighDateTime << 32 | t.dwLowDateTime) for t in times[2:]) / 1e7
    import resource
    child.wait()
    r = resource.getrusage(resource.RUSAGE_CHILDREN)
    total = r.ru_utime + r.ru_stime
    used, cpu_seconds.before = total - getattr(cpu_seconds, "before", 0.0), total
    return used


def replay(variant):
    child = subprocess.Popen([exe, "serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, bufsize=1 << 20)
    spent, counts = defaultdict(float), defaultdict(int)
    one_run = variant in ("run", "plain-run")
    start = time.perf_counter()

    def ask(r):
        t = time.perf_counter()
        child.stdin.write((json.dumps(r) + "\n").encode())
        child.stdin.flush()
        answer = child.stdout.readline()
        took = time.perf_counter() - t
        spent[r["op"]] += took
        counts[r["op"]] += 1
        if r["op"] == "tick" and ticks_out:
            ticks_out.write(json.dumps({"from": r["from"], "ms": took * 1000}) + "\n")
        a = json.loads(answer)
        if answers:
            answers.write(f"{r['op']} {hashlib.sha256(answer).hexdigest()[:16]}\n")
        if "error" in a:
            raise SystemExit(f"{r['op']}: {a['error']}")
        return a

    tick_requests = [r for r in requests if r["op"] == "tick"][:ticks]
    loaded_cycles, done = 0, 0
    for r in requests:
        if r["op"] == "tick":
            if done >= ticks:
                break
            if one_run:
                ask({"op": "run", "id": r["id"], "cycles": tick_requests[-1]["from"] - loaded_cycles})
                break
            done += 1
        if one_run and r["op"] == "sync":
            continue
        r = keep(variant, r)
        if r is None:
            continue
        a = ask(r)
        if r["op"] in ("load", "load_snapshot"):
            loaded_cycles = a.get("cycles", 0)
            if variant in ("plain", "plain-run"):
                # A recording's snapshot carries its call tracker (for watches from it): off, for emulation alone.
                ask({"op": "calls", "id": 0, "on": False})
        if variant == "hash" and r["op"] == "tick":
            ask({"op": "hash", "id": 0})
    wall = time.perf_counter() - start
    used = cpu_seconds(child)
    child.stdin.close()
    child.wait()
    detail = ", ".join(f"{op} {spent[op]:.1f} s/{counts[op]}" for op in sorted(spent, key=lambda o: -spent[o])[:4])
    print(f"{variant:10} {wall:7.1f} s  cpu {used:7.1f} s  ({detail})", flush=True)


for v in variants:
    replay(v)
