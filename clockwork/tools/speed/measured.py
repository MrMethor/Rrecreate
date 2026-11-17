"""Runs a command, and reports its wall time and the peak working set of it and every process under it.

usage: measured.py [--cwd DIR] [--env NAME=VALUE]... -- COMMAND...
"""
import ctypes
import ctypes.wintypes as wt
import os
import subprocess
import sys
import time

k32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)


class PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [("dwSize", wt.DWORD), ("cntUsage", wt.DWORD), ("th32ProcessID", wt.DWORD),
                ("th32DefaultHeapID", ctypes.c_void_p), ("th32ModuleID", wt.DWORD), ("cntThreads", wt.DWORD),
                ("th32ParentProcessID", wt.DWORD), ("pcPriClassBase", ctypes.c_long), ("dwFlags", wt.DWORD),
                ("szExeFile", ctypes.c_wchar * 260)]


class PMC(ctypes.Structure):
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD), ("PeakWorkingSetSize", ctypes.c_size_t),
                ("WorkingSetSize", ctypes.c_size_t), ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t), ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t), ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t)]


k32.CreateToolhelp32Snapshot.restype = wt.HANDLE
k32.OpenProcess.restype = wt.HANDLE


def processes():
    snap = k32.CreateToolhelp32Snapshot(2, 0)
    entry = PROCESSENTRY32W()
    entry.dwSize = ctypes.sizeof(entry)
    found = []
    ok = k32.Process32FirstW(snap, ctypes.byref(entry))
    while ok:
        found.append((entry.th32ProcessID, entry.th32ParentProcessID, entry.szExeFile))
        ok = k32.Process32NextW(snap, ctypes.byref(entry))
    k32.CloseHandle(snap)
    return found


def memory(pid):
    handle = k32.OpenProcess(0x1000 | 0x0010, False, pid)  # QUERY_LIMITED_INFORMATION | VM_READ
    if not handle:
        return None
    counters = PMC()
    counters.cb = ctypes.sizeof(counters)
    ok = psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb)
    k32.CloseHandle(handle)
    return (counters.PeakWorkingSetSize, counters.PeakPagefileUsage) if ok else None


def main():
    args = sys.argv[1:]
    cwd, env = None, dict(os.environ)
    while args and args[0] != "--":
        if args[0] == "--cwd":
            cwd, args = args[1], args[2:]
        elif args[0] == "--env":
            name, value = args[1].split("=", 1)
            env[name], args = value, args[2:]
        else:
            sys.exit(f"unknown {args[0]}")
    command = args[1:]
    start = time.perf_counter()
    child = subprocess.Popen(command, cwd=cwd, env=env)
    peaks = {}
    while child.poll() is None:
        tree, all_ = {child.pid}, processes()
        grew = True
        while grew:
            grew = False
            for pid, parent, exe in all_:
                if parent in tree and pid not in tree:
                    tree.add(pid)
                    grew = True
        for pid, parent, exe in all_:
            if pid in tree:
                m = memory(pid)
                if m:
                    key = f"{exe}:{pid}"
                    old = peaks.get(key, (0, 0))
                    peaks[key] = (max(old[0], m[0]), max(old[1], m[1]))
        time.sleep(0.25)
    wall = time.perf_counter() - start
    print(f"exit      {child.returncode}")
    print(f"wall      {wall:.1f} s")
    for key, (ws, commit) in sorted(peaks.items()):
        print(f"peak      {key}: working set {ws / 2**20:.0f} MB, committed {commit / 2**20:.0f} MB")


if __name__ == "__main__":
    main()
