#!/usr/bin/env python3
"""CPU usage of a process and all its descendants over an interval, plus RSS
and per-process GPU utilisation (nvidia-smi pmon, if available).
usage: proc-usage.py PID [SECONDS]   (WPE_CPU_DETAIL=1: also per process)"""
import os
import subprocess
import sys
import time

TICK = os.sysconf("SC_CLK_TCK")


def tree(root):
    kids = {}
    for d in os.listdir("/proc"):
        if d.isdigit():
            try:
                ppid = int(open(f"/proc/{d}/stat").read().rsplit(")", 1)[1].split()[1])
            except OSError:
                continue
            kids.setdefault(ppid, []).append(int(d))
    out, todo = [], [root]
    while todo:
        p = todo.pop()
        out.append(p)
        todo += kids.get(p, [])
    return out


def ticks(pids):
    t = 0
    for p in pids:
        try:
            f = open(f"/proc/{p}/stat").read().rsplit(")", 1)[1].split()
            t += int(f[11]) + int(f[12])
        except OSError:
            pass
    return t


def rss_mb(pids):
    kb = 0
    for p in pids:
        try:
            kb += int(next(l for l in open(f"/proc/{p}/status") if l.startswith("VmRSS")).split()[1])
        except (OSError, StopIteration):
            pass
    return kb / 1024


root, sec = int(sys.argv[1]), float(sys.argv[2]) if len(sys.argv) > 2 else 5
pids = tree(root)
t0 = ticks(pids)
each0 = {p: ticks([p]) for p in pids}
time.sleep(sec)
pids = tree(root)
if os.environ.get("WPE_CPU_DETAIL"):
    for p in pids:
        c = (ticks([p]) - each0.get(p, 0)) / TICK / sec * 100
        if c >= 0.5:
            try:
                name = open(f"/proc/{p}/comm").read().strip()
            except OSError:
                name = "?"
            print(f"  {p} {name}: {c:.1f}%")
cpu = (ticks(pids) - t0) / TICK / sec * 100
gpu = "n/a"
try:
    out = subprocess.run(["nvidia-smi", "pmon", "-c", "1", "-s", "u"], capture_output=True, text=True, timeout=10).stdout
    sm = [int(l.split()[3]) for l in out.splitlines()
          if l.strip() and not l.startswith("#") and l.split()[1].isdigit() and int(l.split()[1]) in pids
          and l.split()[3].isdigit()]
    gpu = f"{sum(sm)}%" if sm else "0%"
except (OSError, subprocess.SubprocessError, IndexError, ValueError):
    pass
print(f"pid {root} (+{len(pids) - 1} children): cpu {cpu:.1f}% rss {rss_mb(pids):.0f} MB gpu-sm {gpu}")
