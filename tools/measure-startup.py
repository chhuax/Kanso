#!/usr/bin/env python3
"""测量当前 worktree 的 Release App；每次启动使用 target 内独立的 HOME 和数据目录。"""

import argparse
import ctypes
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import time


def require_unlocked_screen():
    graphics = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    foundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    graphics.CGSessionCopyCurrentDictionary.restype = ctypes.c_void_p
    foundation.CFStringCreateWithCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32]
    foundation.CFStringCreateWithCString.restype = ctypes.c_void_p
    foundation.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    foundation.CFDictionaryGetValue.restype = ctypes.c_void_p
    foundation.CFBooleanGetValue.argtypes = [ctypes.c_void_p]
    foundation.CFBooleanGetValue.restype = ctypes.c_bool
    foundation.CFRelease.argtypes = [ctypes.c_void_p]
    session = graphics.CGSessionCopyCurrentDictionary()
    if not session:
        raise RuntimeError("没有可用的图形登录会话，无法测量首帧呈现")
    key = foundation.CFStringCreateWithCString(None, b"CGSSessionScreenIsLocked", 0x08000100)
    value = foundation.CFDictionaryGetValue(session, key)
    locked = value is not None and foundation.CFBooleanGetValue(value)
    foundation.CFRelease(key)
    foundation.CFRelease(session)
    if locked:
        raise RuntimeError("屏幕已锁定，无法测量首帧呈现；请解锁并保持测量窗口可见后重跑")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs 必须大于 0")
    repo = Path(__file__).resolve().parent.parent
    require_unlocked_screen()
    if not args.no_build:
        subprocess.run(["cargo", "xtask", "apple-build", "--configuration", "release"], cwd=repo, check=True)
    executable = repo / "target/apple/DerivedData.noindex/Build/Products/Release-Direct/Kanso.app/Contents/MacOS/Kanso"
    output = repo / "target/performance/startup"
    output.mkdir(parents=True, exist_ok=True)
    results = []
    for run in range(args.runs):
        require_unlocked_screen()
        root = output / f"{time.time_ns()}-{run}"
        root.mkdir()
        env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(root), "CFFIXED_USER_HOME": str(root),
               "TMPDIR": str(root), "KANSO_PERF_ROOT": str(root), "PS1": "kanso-perf-ready> ", "LC_ALL": "en_US.UTF-8"}
        with (root / "app.log").open("w") as log:
            started = time.perf_counter_ns()
            app = subprocess.Popen([str(executable)], cwd=root, env=env, stdout=log, stderr=log, start_new_session=True)
            try:
                while not (root / "ready").exists():
                    if app.poll() is not None:
                        raise RuntimeError(f"App 提前退出，查看 {root / 'app.log'}")
                    time.sleep(0.001)
                ready_ms = (time.perf_counter_ns() - started) / 1e6
                samples = []
                for sample in range(20):
                    time.sleep(0.5)
                    rss = int(subprocess.check_output(["/bin/ps", "-o", "rss=", "-p", str(app.pid)], text=True)) * 1024
                    if sample >= 10:
                        samples.append(rss)
                result = {"run": run + 1, "ready_ms": ready_ms, "rss_bytes": statistics.median(samples),
                          "rss_min_bytes": min(samples), "rss_max_bytes": max(samples), "root": str(root)}
                memory = subprocess.check_output(["/usr/bin/vmmap", "-summary", str(app.pid)], text=True, stderr=subprocess.STDOUT)
                (root / "memory.txt").write_text(memory)
                footprint = re.search(r"^Physical footprint:\s+([\d.]+)([KMG]?)", memory, re.MULTILINE)
                if not footprint:
                    raise RuntimeError(f"vmmap 未返回 physical footprint，查看 {root / 'memory.txt'}")
                result["phys_footprint_bytes"] = float(footprint[1]) * {"": 1, "K": 1024, "M": 1024**2, "G": 1024**3}[footprint[2]]
                phases = json.loads((root / "phases.json").read_text())
                result["renderer"] = json.loads((root / "renderer.json").read_text())
                result["phase_ms"] = {f"{previous['phase']}->{current['phase']}": (current["uptime_ns"] - previous["uptime_ns"]) / 1e6
                                      for previous, current in zip(phases, phases[1:])}
                results.append(result)
                print(f"{run + 1}: 首帧可输入 {ready_ms:.2f} ms，空闲 RSS {result['rss_bytes'] / 1e6:.2f} MB，"
                      f"phys_footprint {result['phys_footprint_bytes'] / 1e6:.2f} MB", flush=True)
            finally:
                if app.poll() is None:
                    os.killpg(app.pid, 15)
                app.wait()
    report = {"runs": results, "median_ready_ms": statistics.median(item["ready_ms"] for item in results),
              "median_rss_bytes": statistics.median(item["rss_bytes"] for item in results),
              "median_phys_footprint_bytes": statistics.median(item["phys_footprint_bytes"] for item in results),
              "median_phase_ms": {phase: statistics.median(item["phase_ms"][phase] for item in results)
                                  for phase in results[0]["phase_ms"]}}
    (output / "latest.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"中位数：{report['median_ready_ms']:.2f} ms，RSS {report['median_rss_bytes'] / 1e6:.2f} MB，"
          f"phys_footprint {report['median_phys_footprint_bytes'] / 1e6:.2f} MB；原始记录：{output / 'latest.json'}")


if __name__ == "__main__":
    main()
