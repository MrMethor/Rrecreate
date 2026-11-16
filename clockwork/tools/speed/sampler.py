"""A sampling profiler for Clockwork on Windows, without admin rights: starts replay.py VARIANT, finds its clockwork
child, samples the instruction pointer of its busiest thread for SECONDS, and prints the functions and the source lines
it was in. SYMBOLS is the folder of clockwork.pdb (a build with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` gives
the lines). dbghelp is the Windows Kits' (the system's is too old for Rust's PDBs), else `DBGHELP`.

usage: [SAMPLE_AFTER=SECONDS] sampler.py CLOCKWORK LOG VARIANT SECONDS SYMBOLS
"""
import collections
import ctypes
import ctypes.wintypes as wt
import os
import subprocess
import sys
import time

exe, log, variant, seconds, symbols = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
k32 = ctypes.WinDLL("kernel32", use_last_error=True)
KITS = r"C:\Program Files (x86)\Windows Kits\10\Debuggers\x64\dbghelp.dll"
dbghelp = ctypes.WinDLL(os.environ.get("DBGHELP", KITS), use_last_error=True)
dbghelp.SymInitializeW.argtypes = [wt.HANDLE, wt.LPCWSTR, wt.BOOL]
dbghelp.SymFromAddrW.argtypes = [wt.HANDLE, ctypes.c_uint64, ctypes.POINTER(ctypes.c_uint64), ctypes.c_void_p]
dbghelp.SymGetLineFromAddr64.argtypes = [wt.HANDLE, ctypes.c_uint64, ctypes.POINTER(wt.DWORD), ctypes.c_void_p]
k32.SuspendThread.argtypes = [wt.HANDLE]
k32.ResumeThread.argtypes = [wt.HANDLE]
k32.GetThreadContext.argtypes = [wt.HANDLE, ctypes.c_void_p]
k32.GetThreadTimes.argtypes = [wt.HANDLE] + [ctypes.c_void_p] * 4
winmm = ctypes.WinDLL("winmm")
k32.OpenProcess.restype = wt.HANDLE
k32.OpenThread.restype = wt.HANDLE
k32.CreateToolhelp32Snapshot.restype = wt.HANDLE


class PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [("dwSize", wt.DWORD), ("cntUsage", wt.DWORD), ("th32ProcessID", wt.DWORD),
                ("th32DefaultHeapID", ctypes.c_void_p), ("th32ModuleID", wt.DWORD), ("cntThreads", wt.DWORD),
                ("th32ParentProcessID", wt.DWORD), ("pcPriClassBase", ctypes.c_long), ("dwFlags", wt.DWORD),
                ("szExeFile", ctypes.c_wchar * 260)]


class THREADENTRY32(ctypes.Structure):
    _fields_ = [("dwSize", wt.DWORD), ("cntUsage", wt.DWORD), ("th32ThreadID", wt.DWORD),
                ("th32OwnerProcessID", wt.DWORD), ("tpBasePri", ctypes.c_long), ("tpDeltaPri", ctypes.c_long),
                ("dwFlags", wt.DWORD)]


def children(parent):
    snap = k32.CreateToolhelp32Snapshot(2, 0)
    e = PROCESSENTRY32W()
    e.dwSize = ctypes.sizeof(e)
    found = []
    ok = k32.Process32FirstW(snap, ctypes.byref(e))
    while ok:
        if e.th32ParentProcessID == parent:
            found.append((e.th32ProcessID, e.szExeFile))
        ok = k32.Process32NextW(snap, ctypes.byref(e))
    k32.CloseHandle(snap)
    return found


def threads(pid):
    snap = k32.CreateToolhelp32Snapshot(4, 0)
    e = THREADENTRY32()
    e.dwSize = ctypes.sizeof(e)
    found = []
    ok = k32.Thread32First(snap, ctypes.byref(e))
    while ok:
        if e.th32OwnerProcessID == pid:
            found.append(e.th32ThreadID)
        ok = k32.Thread32Next(snap, ctypes.byref(e))
    k32.CloseHandle(snap)
    return found


def cpu_time(handle):
    c, x, kt, ut = (wt.FILETIME() for _ in range(4))
    k32.GetThreadTimes(handle, ctypes.byref(c), ctypes.byref(x), ctypes.byref(kt), ctypes.byref(ut))
    return (ut.dwHighDateTime << 32) | ut.dwLowDateTime


replay = subprocess.Popen([sys.executable, __file__.replace("sampler.py", "replay.py"), exe, log, "--ticks", "100000",
                           variant], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pid = None
for _ in range(100):
    time.sleep(0.1)
    kids = [p for p, name in children(replay.pid) if name.lower() == "clockwork.exe"]
    if kids:
        pid = kids[0]
        break
if pid is None:
    sys.exit("no clockwork child")
time.sleep(float(os.environ.get("SAMPLE_AFTER", "3")))  # into the run: a stretch of it
# The busiest thread: the most user time over a second.
handles = {t: k32.OpenThread(0x0002 | 0x0008 | 0x0040, False, t) for t in threads(pid)}  # SUSPEND|GET_CONTEXT|QUERY
before = {t: cpu_time(h) for t, h in handles.items()}
time.sleep(1)
busiest = max(handles, key=lambda t: cpu_time(handles[t]) - before[t])
thread = handles[busiest]

buf = ctypes.create_string_buffer(1232 + 16)
base = (ctypes.addressof(buf) + 15) & ~15
ctx = (ctypes.c_char * 1232).from_address(base)
winmm.timeBeginPeriod(1)
samples = collections.Counter()
end = time.perf_counter() + seconds
while time.perf_counter() < end:
    if k32.SuspendThread(thread) != 0xFFFFFFFF:
        ctypes.memset(base, 0, 1232)
        ctypes.c_uint32.from_address(base + 0x30).value = 0x100001  # CONTEXT_AMD64 | CONTEXT_CONTROL
        if k32.GetThreadContext(thread, ctypes.c_void_p(base)):
            samples[ctypes.c_uint64.from_address(base + 0xF8).value] += 1
        k32.ResumeThread(thread)
    time.sleep(0.001)
winmm.timeEndPeriod(1)

process = k32.OpenProcess(0x0400 | 0x0010, False, pid)
dbghelp.SymSetOptions(0x2 | 0x4 | 0x10)  # UNDNAME | DEFERRED_LOADS | LOAD_LINES
if not dbghelp.SymInitializeW(process, symbols, True):
    sys.exit(f"dbghelp: error {ctypes.get_last_error()}")


class SYMBOL_INFOW(ctypes.Structure):
    _fields_ = [("SizeOfStruct", wt.ULONG), ("TypeIndex", wt.ULONG), ("Reserved", ctypes.c_uint64 * 2),
                ("Index", wt.ULONG), ("Size", wt.ULONG), ("ModBase", ctypes.c_uint64), ("Flags", wt.ULONG),
                ("Value", ctypes.c_uint64), ("Address", ctypes.c_uint64), ("Register", wt.ULONG),
                ("Scope", wt.ULONG), ("Tag", wt.ULONG), ("NameLen", wt.ULONG), ("MaxNameLen", wt.ULONG),
                ("Name", ctypes.c_wchar * 1024)]


class LINE64(ctypes.Structure):
    _fields_ = [("SizeOfStruct", wt.DWORD), ("Key", ctypes.c_void_p), ("LineNumber", wt.DWORD),
                ("FileName", ctypes.c_char_p), ("Address", ctypes.c_uint64)]


by_function, by_line = collections.Counter(), collections.Counter()
for addr, n in samples.items():
    s = SYMBOL_INFOW()
    s.SizeOfStruct = 88
    s.MaxNameLen = 1024
    disp = ctypes.c_uint64()
    name = s.Name if dbghelp.SymFromAddrW(process, ctypes.c_uint64(addr), ctypes.byref(disp), ctypes.byref(s)) \
        else f"?{addr:X}"
    by_function[name] += n
    line = LINE64()
    line.SizeOfStruct = ctypes.sizeof(LINE64)
    d32 = wt.DWORD()
    if dbghelp.SymGetLineFromAddr64(process, ctypes.c_uint64(addr), ctypes.byref(d32), ctypes.byref(line)):
        file = line.FileName.decode(errors="replace").replace("\\", "/").split("/src/")[-1]
        by_line[f"{file}:{line.LineNumber}"] += n
replay.kill()
for p, _ in children(replay.pid):
    subprocess.run(["taskkill", "/F", "/PID", str(p)], capture_output=True)
total = sum(samples.values())
print(f"samples   {total} of thread {busiest}, variant {variant}")
for name, n in by_function.most_common(25):
    print(f"{100 * n / total:5.1f}%  {name[:150]}")
if by_line:
    print("lines:")
    for name, n in by_line.most_common(25):
        print(f"{100 * n / total:5.1f}%  {name}")
