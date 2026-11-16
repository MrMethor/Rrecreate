"""Records every session of a port again, each after the session it is based on, and says whether each trace came out
the same: the check that a change to Clockwork changed nothing it records (M6.5's step 1). Each session's time is
measured; a session whose trace differs is listed at the end.

usage: rerecord.py PORT RRC [--clockwork EXE] [--log FILE] [--only NAME,...]
"""
import os
import re
import subprocess
import sys
import time

port, rrc = sys.argv[1], sys.argv[2]
arg = lambda name: sys.argv[sys.argv.index(name) + 1] if name in sys.argv else None
env = dict(os.environ)
if arg("--clockwork"):
    env["RRC_CLOCKWORK"] = arg("--clockwork")
log = open(arg("--log"), "a", encoding="utf-8") if arg("--log") else None
only = set(arg("--only").split(",")) if arg("--only") else None

sessions = {}
for name in os.listdir(os.path.join(port, "sessions")):
    if name.endswith(".toml"):
        text = open(os.path.join(port, "sessions", name), encoding="utf-8").read()
        base = re.search(r'^\[from\]\s*\nsession = "([^"]+)"', text, re.M)
        sessions[name[:-5]] = base.group(1) if base else None
order, placed = [], set()


def place(name):
    if name in placed or name not in sessions:
        return
    if sessions[name]:
        place(sessions[name])
    placed.add(name)
    order.append(name)


for name in sorted(sessions):
    place(name)
if only:
    order = [name for name in order if name in only]


def say(line):
    print(line, flush=True)
    if log:
        log.write(line + "\n")
        log.flush()


differ, total = [], 0.0
for n, name in enumerate(order, 1):
    start = time.perf_counter()
    done = subprocess.run([rrc, "record", name, "--run"], cwd=port, env=env, capture_output=True, text=True)
    took = time.perf_counter() - start
    total += took
    out = done.stdout + done.stderr
    same = "same      as the trace recorded before" in out
    verdict = "same" if same else "DIFFERS" if done.returncode == 0 else "FAILED"
    if not same:
        differ.append(name)
    say(f"{n:3}/{len(order)} {name:12} {took:6.1f} s  {verdict}")
    if not same:
        say("          " + " | ".join(line for line in out.splitlines() if line.split(" ")[0] in
                                     ("differs", "same", "error", "rrc:", "runs", "trace")))
say(f"done      {len(order)} sessions in {total / 60:.1f} min; {len(differ)} not the same: {', '.join(differ) or 'none'}")
