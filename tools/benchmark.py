"""Drive the Windows GUI against the independent localhost fixture."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tomllib

from join_capture import parts_for, rows


def run(exe, root, name, schedule, mode):
    directory = root / name
    directory.mkdir(parents=True)
    adapter_report = directory / "adapter.json"
    adapter = subprocess.Popen([
        sys.executable, str(Path(__file__).with_name("loopback_adapter.py")),
        str(adapter_report), "--mode", mode,
    ], stdout=subprocess.PIPE, text=True)
    try:
        if adapter.stdout.readline().strip() != "Ready on loopback":
            raise RuntimeError("Loopback adapter failed to start")
        subprocess.run([exe, "--loopback-benchmark", schedule, directory], check=True, timeout=35)
        if adapter.wait(timeout=10) != 0:
            raise RuntimeError("Loopback adapter failed")
    finally:
        if adapter.poll() is None:
            adapter.terminate()
            adapter.wait()
    with (directory / "scanner.toml").open("rb") as file:
        report = tomllib.load(file)
    first = next(directory.glob("*-000001.csv"))
    parts = parts_for(first.with_name(first.name.removesuffix("-000001.csv")))
    samples = list(rows(parts))
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--full", action="store_true")
    args = parser.parse_args()
    cases = [("deadline", "deadline", "normal"), ("tokio", "tokio", "normal")]
    if args.full:
        cases.extend((mode, "deadline", mode) for mode in ("faults", "stall", "reject", "different-api"))
        cases.extend((mode, mode, mode) for mode in ("owner", "reconnect"))
    for name, schedule, mode in cases:
        run(args.exe.resolve(), args.directory.resolve(), name, schedule, mode)


if __name__ == "__main__":
    main()
