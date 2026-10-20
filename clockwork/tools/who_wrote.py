"""Who wrote a variable, and when: an example of driving `clockwork serve`.

    python clockwork/tools/who_wrote.py <dir> <program> --sync +0A31C --var +21C40:2 [--tick N]
        [--seconds 90] [--clockwork target/release/clockwork] [-- run options...]

The sync point marks the game's ticks (an instruction run once per frame, such as where the main
loop's timer wait returns); the variable is ADDRESS:SIZE. Without --tick, every write in the run is listed with the tick
it happened in. With --tick N, the run stops at tick N, watches the variable through that tick only,
and says who wrote it: the instruction, the bytes, and the call chain. Options after `--` go to the
program's start as `clockwork run` takes them (an --input-file walkthrough, a CD drive...).
"""

import argparse
import bisect
import json
import os
import subprocess
import sys


class Clockwork:
    def __init__(self, path):
        self.process = subprocess.Popen([os.path.abspath(path), "serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.next_id = 0

    def __call__(self, op, **fields):
        self.next_id += 1
        self.process.stdin.write(json.dumps({"id": self.next_id, "op": op, **fields}) + "\n")
        self.process.stdin.flush()
        answer = json.loads(self.process.stdout.readline())
        if "error" in answer:
            sys.exit(f"clockwork: {op}: {answer['error']}")
        return answer

    def close(self):
        self("quit")
        self.process.wait()


def chain(frames):
    return " <- ".join(f"{f['entry']} (from {f['from']}{', interrupt' if f['interrupt'] else ''})" for f in frames) or "-"


def by(hit):
    b = hit["by"]
    if "instruction" in b:
        return b["instruction"]
    return f"service {b['service']} AX={b['ax']}" if "service" in b else "a device"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dir")
    parser.add_argument("program")
    parser.add_argument("--sync", required=True, help="the instruction that marks a tick (+OFF)")
    parser.add_argument("--var", required=True, help="ADDRESS:SIZE (size in bytes, decimal)")
    parser.add_argument("--tick", type=int, help="the tick to explain (1 = the first)")
    parser.add_argument("--seconds", type=int, default=90, help="emulated seconds at most")
    default = "target/release/clockwork" + (".exe" if os.name == "nt" else "")
    parser.add_argument("--clockwork", default=default)
    argv = sys.argv[1:]
    split = argv.index("--") if "--" in argv else len(argv)
    a = parser.parse_args(argv[:split])
    options = argv[split + 1:]
    at, size = a.var.rsplit(":", 1)
    cw = Clockwork(a.clockwork)
    cw("load", argv=[a.dir, a.program, *options])
    if a.tick is None:
        cw("sync", at=a.sync, log=True)
        cw("watch", ranges=[{"at": at, "len": int(size)}])
        stop = cw("run", ms=a.seconds * 1000)["stop"]
        hits = cw("watch_end")["hits"]
        ticks = cw("syncs", at=a.sync)["cycles"]
        print(f"{len(hits)} writes in {len(ticks)} ticks ({stop})")
        for hit in hits:
            tick = bisect.bisect_right(ticks, hit["cycle"])
            print(f"tick {tick:6}  {hit['at']} = {hit['value']:02X} (was {hit['old']:02X})  by {by(hit)}  {chain(hit['chain'])}")
    else:
        cw("calls", on=True)  # so the watch, started at the tick, knows the calls in progress
        cw("sync", at=a.sync)
        reached = cw("run", sync=a.sync, n=a.tick, max_ms=a.seconds * 1000)
        if "sync" not in reached:
            sys.exit(f"tick {a.tick} not reached: {reached['stop']}")
        cw("watch", ranges=[{"at": at, "len": int(size)}])
        cw("run", sync=a.sync, n=1, max_ms=a.seconds * 1000)
        hits = cw("watch_end")["hits"]
        print(f"tick {a.tick} (from {reached['ms']} ms): {len(hits)} writes to {at}")
        for hit in hits:
            print(f"  {hit['at']} = {hit['value']:02X} (was {hit['old']:02X})  by {by(hit)}")
            print(f"    called {chain(hit['chain'])}")
    cw.close()


if __name__ == "__main__":
    main()
