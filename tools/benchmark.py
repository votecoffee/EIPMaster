"""Drive the Windows GUI against the independent localhost fixture."""
import argparse
from contextlib import contextmanager, ExitStack
from datetime import datetime, timedelta
import json
from pathlib import Path
import subprocess
import sys
import tomllib

from join_capture import parts_for, rows


@contextmanager
def loopback(directory, mode, address="127.0.0.1"):
    directory.mkdir(parents=True)
    adapter_report = directory / "adapter.json"
    adapter = subprocess.Popen([
        sys.executable, str(Path(__file__).with_name("loopback_adapter.py")),
        str(adapter_report), "--mode", mode, "--address", address,
    ], stdout=subprocess.PIPE, text=True)
    try:
        if adapter.stdout.readline().strip() != "Ready on loopback":
            raise RuntimeError("Loopback adapter failed to start")
        yield adapter_report
        if adapter.wait(timeout=10) != 0:
            raise RuntimeError("Loopback adapter failed")
    finally:
        if adapter.poll() is None:
            adapter.terminate()
            adapter.wait()


def read_capture(directory):
    with (directory / "scanner.toml").open("rb") as file:
        report = tomllib.load(file)
    first = next(directory.glob("*-000001.csv"))
    parts = parts_for(first.with_name(first.name.removesuffix("-000001.csv")))
    return report, parts, list(rows(parts))


def run(exe, root, name, schedule, mode):
    directory = root / name
    with loopback(directory, mode) as adapter_report:
        subprocess.run([exe, "--loopback-benchmark", schedule, directory], check=True, timeout=35)
    report, parts, samples = read_capture(directory)
    assert len(samples) == report["written"] == report["seen"]
    if mode != "reconnect":
        assert len(samples) == report["valid"]
    assert [int(row[2]) for row in samples] == list(range(len(samples)))
    assert all(int(row[6]) == int(row[4]) // 16 for row in samples)
    assert not report["disk_error"] and report["overruns"] == 0
    if mode == "reject":
        assert report["state"] == "Error" and report["valid"] == 0
        assert "0x01" in report["cip_error"] and "0x0109" in report["cip_error"]
        assert not report["network_error"]
    elif mode == "stall":
        assert report["state"] == "Timed out" and report["timeouts"] == 1
    else:
        assert report["state"] == "Closed" and len(parts) >= 3
        assert len(samples) > 100 and len(set(row[6] for row in samples)) < len(samples)
    if mode == "faults":
        assert all(report[key] >= 1 for key in ("gaps", "duplicates", "late", "invalid"))
        assert not any(int(row[4]) == 60 for row in samples)
    if mode == "different-api":
        assert (report["actual_ot_us"], report["actual_to_us"]) == (1250, 1500)
    adapter_data = json.loads(adapter_report.read_text(encoding="utf-8"))
    if mode != "reject":
        assert adapter_data["forward_close"] and adapter_data["ot_invalid"] == 0
    if mode == "owner":
        assert adapter_data["output_values"] == ["1234", "5678"]
    if mode == "reconnect":
        assert {int(row[3]) for row in samples} == {1, 2} and report["generations"] == 2
    print(name, report, flush=True)


def capture_time(row):
    return datetime.fromisoformat(row[0]) + timedelta(microseconds=int(row[1]))


def run_dual(exe, root):
    scenarios = [
        ("healthy", "normal", "127.0.0.1", 8000, "Closed"),
        ("stalled", "stall", "127.0.0.2", 16000, "Timed out"),
    ]
    with ExitStack() as stack:
        for name, mode, address, *_ in scenarios:
            stack.enter_context(loopback(root / name, mode, address))
        subprocess.run([exe, "--loopback-benchmark", "dual", root], check=True, timeout=35)

    streams = []
    for name, mode, address, rpi, state in scenarios:
        directory = root / name
        report, parts, samples = read_capture(directory)
        adapter = json.loads((directory / "adapter.json").read_text(encoding="utf-8"))
        assert report["adapter_ip"] == address and report["identity_serial"] == adapter["serial_number"]
        assert report["state"] == state and report["timeouts"] == (mode == "stall")
        assert all(report[key] == rpi for key in ("requested_ot_us", "requested_to_us",
                                                   "actual_ot_us", "actual_to_us"))
        assert report["generations"] == 1 and not report["disk_error"] and report["overruns"] == 0
        assert adapter["forward_close"] and adapter["ot_invalid"] == 0
        assert len(samples) == report["written"] == report["seen"] == report["valid"] > 30
        assert [int(row[2]) for row in samples] == list(range(len(samples)))
        assert all(int(row[6]) == adapter["value_base"] + int(row[4]) // 16 for row in samples)
        streams.append(samples)
        print(f"{name}: {state}, {len(samples)} samples in {len(parts)} CSV parts, "
              f"{report['gaps']} sequence gaps, {report['overruns']} overruns", flush=True)
    healthy, stalled = streams
    assert capture_time(healthy[0]) < capture_time(stalled[-1])
    assert capture_time(stalled[0]) < capture_time(healthy[-1])
    assert capture_time(healthy[-1]) - capture_time(stalled[-1]) > timedelta(seconds=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    parser.add_argument("directory", type=Path)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--full", action="store_true")
    modes.add_argument("--dual-only", action="store_true")
    args = parser.parse_args()
    if args.dual_only:
        run_dual(args.exe.resolve(), args.directory.resolve() / "dual")
        return
    cases = [("deadline", "deadline", "normal"), ("tokio", "tokio", "normal")]
    if args.full:
        cases.extend((mode, "deadline", mode) for mode in ("faults", "stall", "reject", "different-api"))
        cases.extend((mode, mode, mode) for mode in ("owner", "reconnect"))
    for name, schedule, mode in cases:
        run(args.exe.resolve(), args.directory.resolve(), name, schedule, mode)
    if args.full:
        run_dual(args.exe.resolve(), args.directory.resolve() / "dual")


if __name__ == "__main__":
    main()
