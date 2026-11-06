"""Verify Windows console stops with an isolated windowless console and localhost adapter."""
import argparse
from contextlib import ExitStack, contextmanager
import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import subprocess
import sys
import time
import tomllib

from join_capture import parts_for, rows
from test_cli import PROFILE
from test_recovery import adapter, fixture_report

CTRL_C = 0
CTRL_BREAK = 1
CONSOLE_NO_WINDOW = 2
NEW_CONSOLE = 1


class ConsoleOptions(ctypes.Structure):
    _fields_ = [("mode", ctypes.c_int), ("use_show_window", wintypes.BOOL),
                ("show_window", wintypes.WORD)]


def console_processes(kernel):
    pids = (wintypes.DWORD * 4)()
    count = kernel.GetConsoleProcessList(pids, len(pids))
    if count > len(pids):
        raise RuntimeError("Unexpected processes attached to the test console")
    return set(pids[:count])


@contextmanager
def private_console():
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    # Detach only this test process. The parent shell keeps its console.
    if console_processes(kernel) and not kernel.FreeConsole():
        raise ctypes.WinError(ctypes.get_last_error())
    allocate = kernel.AllocConsoleWithOptions
    allocate.argtypes = [ctypes.POINTER(ConsoleOptions), ctypes.POINTER(ctypes.c_int)]
    allocate.restype = ctypes.c_long
    result = ctypes.c_int()
    status = allocate(ctypes.byref(ConsoleOptions(CONSOLE_NO_WINDOW, False, 0)), ctypes.byref(result))
    if status != 0 or result.value != NEW_CONSOLE:
        raise RuntimeError(f"Could not allocate a private windowless console ({status:#x})")
    try:
        assert console_processes(kernel) == {os.getpid()}
        handler_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.DWORD)
        handler = handler_type(lambda event: event in (CTRL_C, CTRL_BREAK))
        kernel.SetConsoleCtrlHandler.argtypes = [handler_type, wintypes.BOOL]
        if not kernel.SetConsoleCtrlHandler(handler, True):
            raise ctypes.WinError(ctypes.get_last_error())
        yield kernel
    finally:
        kernel.FreeConsole()


def wait_for_capture(proc, capture, minimum_parts):
    deadline = time.monotonic() + 4
    while time.monotonic() < deadline:
        assert proc.poll() is None, "CLI stopped before the test signal"
        if sum(part.stat().st_size > 256 for part in capture.glob("*.csv")) >= minimum_parts:
            return
        time.sleep(0.01)
    raise RuntimeError("Capture did not write input samples before the test signal")


def send_interrupt(kernel, proc, event):
    assert proc.poll() is None
    if console_processes(kernel) != {os.getpid(), proc.pid}:
        raise RuntimeError("Refusing to signal a console shared with another process")
    if not kernel.GenerateConsoleCtrlEvent(event, 0):
        raise ctypes.WinError(ctypes.get_last_error())


def interrupted_capture(kernel, exe, root, profile, case, command, event, fixture_mode="normal"):
    directory = root / case
    capture = directory / "baseline-8ms" if command == "qualify" else directory
    if command == "qualify":
        args = [exe, command, profile, directory, "loopback", "0:12:1:305419896", "10"]
    else:
        args = [exe, command, profile, directory, "10", "0:12:1"]
    with ExitStack() as stack:
        fixture = adapter(stack, root, case, "127.0.0.1", fixture_mode)
        with subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True) as proc:
            try:
                wait_for_capture(proc, capture, minimum_parts=2 if event == CTRL_BREAK else 1)
                if fixture_mode == "stall":
                    target = fixture_report(fixture)
                send_interrupt(kernel, proc, event)
                stdout, stderr = proc.communicate(timeout=8)
                assert proc.returncode == 130, (proc.returncode, stdout, stderr)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait(timeout=5)
        if fixture_mode != "stall":
            target = fixture_report(fixture)
    assert target["forward_close"]
    result = tomllib.loads(stdout)
    if command == "qualify":
        assert result == tomllib.loads((directory / "qualification.toml").read_text(encoding="utf-8"))
        assert result["outcome"] == "interrupted" and "current_run" not in result
        assert [run["name"] for run in result["runs"]] == ["baseline-8ms"]
        assert "Capture interrupted by console signal" in result["runs"][0]["errors"]
        assert not (directory / "trial-1").exists()
        result = result["runs"][0]["capture"]
    assert result == tomllib.loads((capture / "capture.toml").read_text(encoding="utf-8"))
    assert result["interrupted"] and not result["duration_reached"]
    assert result["state"] == ("Timed out" if fixture_mode == "stall" else "Closed")
    assert result["forward_close"] == {"outcome": "confirmed"}
    assert result["written"] == result["seen"] == result["accepted"] > 0
    assert result["after_capture_deadline"] == 0
    assert result["buffer_overruns"] == 0 and "disk_error" not in result
    if fixture_mode == "stall":
        assert len(result["attempts"]) == 1 and result["timeouts"] == 1
    parts = parts_for(Path(result["csv_prefix"]))
    assert event != CTRL_BREAK or len(parts) >= 2
    captured = list(rows(parts))
    assert [int(row[2]) for row in captured] == list(range(result["written"]))
    print(f"{case}: exit 130, Forward_Close received, {len(captured)} CSV rows in {len(parts)} parts",
          flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--private-console", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if sys.platform != "win32" or sys.getwindowsversion().build < 26100:
        parser.error("The windowless console fixture requires Windows 11 build 26100 or later")
    exe = args.exe.resolve()
    root = args.directory.resolve()
    if not args.private_console:
        result = subprocess.run(
            [sys.executable, Path(__file__).resolve(), exe, root, "--private-console"],
            creationflags=subprocess.CREATE_NO_WINDOW, capture_output=True, text=True, timeout=35,
        )
        print(result.stdout, end="")
        print(result.stderr, end="", file=sys.stderr)
        return result.returncode
    root.mkdir(parents=True)
    profile = root / "8ms.toml"
    text = PROFILE.read_text(encoding="utf-8").replace("rpi_us = 1000", "rpi_us = 8000")
    profile.write_text(text + "\n[app]\nrotate_seconds = 1\n", encoding="utf-8")
    recover = root / "reconnect.toml"
    recover.write_text(text + "\n[app]\nauto_reconnect = true\n", encoding="utf-8")
    with private_console() as kernel:
        interrupted_capture(kernel, exe, root, profile, "capture-break", "capture", CTRL_BREAK)
        interrupted_capture(kernel, exe, root, profile, "qualification-ctrl-c", "qualify", CTRL_C)
        interrupted_capture(kernel, exe, root, recover, "retry-stop", "capture", CTRL_C, "stall")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
