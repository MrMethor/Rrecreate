"""Runs a command, and reports how the memory of it and of every process under it went over the run (M6.5's step 5):
each process's committed memory and working set at a tenth, a half and nine tenths of its life and at its end, how
fast its committed memory grew over the second half, and its committed memory at each minute. Memory that stays flat
grows by little there.

usage: memory.py [--cwd DIR] [--every SECONDS] -- COMMAND...
"""
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from measured import PMC, k32, processes, psapi  # noqa: E402  (measured.py: the same Windows calls)
import ctypes  # noqa: E402


def now(pid):
    """The process's working set and committed memory now, in bytes."""
    handle = k32.OpenProcess(0x1000 | 0x0010, False, pid)
    if not handle:
        return None
    counters = PMC()
    counters.cb = ctypes.sizeof(counters)
    ok = psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb)
    k32.CloseHandle(handle)
    return (counters.WorkingSetSize, counters.PagefileUsage) if ok else None


def main():
    args = sys.argv[1:]
    cwd, every = None, 2.0
    while args and args[0] != "--":
        if args[0] == "--cwd":
            cwd, args = args[1], args[2:]
        elif args[0] == "--every":
            every, args = float(args[1]), args[2:]
        else:
            sys.exit(f"unknown {args[0]}")
    start = time.perf_counter()
    child = subprocess.Popen(args[1:], cwd=cwd)
    samples = {}
    while child.poll() is None:
        tree, all_ = {child.pid}, processes()
        grew = True
        while grew:
            grew = False
            for pid, parent, _ in all_:
                if parent in tree and pid not in tree:
                    tree.add(pid)
                    grew = True
        t = time.perf_counter() - start
        for pid, _, exe in all_:
            if pid in tree:
                m = now(pid)
                if m:
                    samples.setdefault(f"{exe}:{pid}", []).append((t, *m))
        time.sleep(every)
    print(f"exit      {child.returncode}")
    print(f"wall      {time.perf_counter() - start:.1f} s")
    mb = 2**20
    for key, s in sorted(samples.items(), key=lambda kv: kv[1][0][0]):
        if len(s) < 4:
            continue
        at = lambda f: s[min(len(s) - 1, int(len(s) * f))]
        points = "  ".join(f"{p[0]:.0f}s {p[2] / mb:.0f}/{p[1] / mb:.0f}" for p in (at(0.1), at(0.5), at(0.9), s[-1]))
        half = s[len(s) // 2:]
        span = (half[-1][0] - half[0][0]) / 60 or 1
        slope = (half[-1][2] - half[0][2]) / mb / span
        print(f"memory    {key} (committed/working set, MB): {points}; second half {slope:+.1f} MB a minute")
        minutes = [next(p for p in s if p[0] >= m * 60) for m in range(1, int(s[-1][0] // 60) + 1)]
        if minutes:
            print(f"minutes   {key}: committed MB at each minute: {' '.join(f'{p[2] / mb:.0f}' for p in minutes)}")


if __name__ == "__main__":
    main()
