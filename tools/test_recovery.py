"""Verify per-device recovery and disk failure isolation without opening a GUI."""
import argparse
from contextlib import contextmanager, ExitStack
from datetime import datetime, timedelta
import json
from pathlib import Path
import subprocess
import sys
import time
import tomllib

from join_capture import parts_for, rows

HERE = Path(__file__).resolve().parent
PROFILE = HERE.parent / "samples" / "loopback.toml"


@contextmanager
def process(command):
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                          creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0)) as proc:
        try:
            yield proc
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)


def adapter(stack, root, name, address, mode):
    path = root / f"{name}-adapter.json"
    proc = stack.enter_context(process([
        sys.executable, HERE / "loopback_adapter.py", path, "--address", address, "--mode", mode,
    ]))
    assert proc.stdout.readline().strip() == "Ready on loopback"
    return proc, path


def capture(stack, exe, root, name, address, seconds, rotate_seconds):
    profile = root / f"{name}.toml"
    text = (PROFILE.read_text(encoding="utf-8")
            .replace('device_ip = "127.0.0.1"', f'device_ip = "{address}"')
            .replace("rpi_us = 1000", "rpi_us = 8000"))
    profile.write_text(text + f"\n[app]\nauto_reconnect = true\nrotate_seconds = {rotate_seconds}\n",
                       encoding="utf-8")
    return stack.enter_context(process([
        exe, "capture", profile, root / name, str(seconds), "0:12:1",
    ]))


def receipt(proc, root, name, code):
    stdout, stderr = proc.communicate(timeout=20)
    assert proc.returncode == code, (name, stdout, stderr)
    report = tomllib.loads(stdout)
    assert report == tomllib.loads((root / name / "capture.toml").read_text(encoding="utf-8"))
    assert report["buffer_overruns"] == 0
    return report


def fixture_report(run):
    proc, path = run
    _, error = proc.communicate(timeout=5)
    assert proc.returncode == 0, error
    return json.loads(path.read_text(encoding="utf-8"))


def inject_rotation_collision(directory):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        first = next(directory.glob("*-000001.csv"), None)
        if first is not None:
            target = first.with_name(first.name.removesuffix("-000001.csv") + "-000002.csv")
            with target.open("xb") as file:
                file.write(b"existing file")
            return first, target
        time.sleep(0.01)
    raise AssertionError("Capture never opened its first CSV part")


def samples(report, serial_number, value_base, parts):
    captured = list(rows(parts))
    assert report["identity"]["serial"] == serial_number
    assert len(captured) == report["written"] > 0
    assert [int(row[2]) for row in captured] == list(range(len(captured)))
    assert len({row[0] for row in captured}) == 1
    assert all(int(row[6]) == value_base + int(row[4]) // 16 for row in captured)
    return captured


def timestamp(row):
    return datetime.fromisoformat(row[0]) + timedelta(microseconds=int(row[1]))


def concurrent(exe, root):
    with ExitStack() as stack:
        healthy_fixture = adapter(stack, root, "healthy", "127.0.0.1", "normal")
        recovered_fixture = adapter(stack, root, "recovered", "127.0.0.2", "recover")
        disk_fixture = adapter(stack, root, "disk", "127.0.0.3", "normal")
        healthy_run = capture(stack, exe, root, "healthy", "127.0.0.1", 6, 1)
        recovered_run = capture(stack, exe, root, "recovered", "127.0.0.2", 5, 1)
        disk_run = capture(stack, exe, root, "disk", "127.0.0.3", 5, 2)
        first, sentinel = inject_rotation_collision(root / "disk")
        healthy = receipt(healthy_run, root, "healthy", 0)
        recovered = receipt(recovered_run, root, "recovered", 0)
        disk = receipt(disk_run, root, "disk", 1)
        healthy_adapter = fixture_report(healthy_fixture)
        recovered_adapters = fixture_report(recovered_fixture)["attempts"]
        disk_adapter = fixture_report(disk_fixture)

    assert healthy["state"] == recovered["state"] == "Closed"
    assert healthy["timeouts"] == 0 and recovered["timeouts"] == 1
    assert len(healthy["attempts"]) == len(disk["attempts"]) == 1
    prior, restored = recovered["attempts"]
    assert (prior["state"], restored["state"]) == ("Timed out", "Closed")
    assert (prior["generation"], restored["generation"]) == (1, 2)
    timeout = prior["receive_timeout"]
    assert not timeout["waiting_for_first_input"]
    assert timeout["limit_us"] == 256_000
    assert 0 < timeout["last_sequence"] <= recovered_adapters[0]["to_last_sequence"] == 75
    assert timeout["valid_input_silence_us"] >= timeout["limit_us"]
    assert timeout["last_udp_age_us"] <= timeout["valid_input_silence_us"]
    assert "receive_timeout" not in restored and "receive_timeout" not in recovered
    assert recovered["accepted"] == recovered["written"] == prior["accepted"] + restored["accepted"]
    assert all("disk_error" not in report for report in (healthy, recovered))
    assert "disk_error" in disk and not disk["duration_reached"]
    assert disk["seen"] > disk["written"]
    assert sentinel.read_bytes() == b"existing file"
    assert all(a["forward_close"] and a["ot_invalid"] == 0
               for a in (healthy_adapter, *recovered_adapters, disk_adapter))
    assert recovered_adapters[0]["serial_number"] == recovered_adapters[1]["serial_number"]

    healthy_parts = parts_for(Path(healthy["csv_prefix"]))
    recovered_parts = parts_for(Path(recovered["csv_prefix"]))
    assert len(healthy_parts) > 1 and len(recovered_parts) > 1
    h = samples(healthy, healthy_adapter["serial_number"], healthy_adapter["value_base"], healthy_parts)
    r = samples(recovered, recovered_adapters[0]["serial_number"],
                recovered_adapters[0]["value_base"], recovered_parts)
    d = samples(disk, disk_adapter["serial_number"], disk_adapter["value_base"], [first])
    assert {row[3] for row in h} == {row[3] for row in d} == {"1"}
    first_recovered = next(index for index, row in enumerate(r) if row[3] == "2")
    assert first_recovered == prior["accepted"] and {row[3] for row in r} == {"1", "2"}
    assert int(r[first_recovered - 1][4]) == timeout["last_sequence"]
    assert timestamp(r[first_recovered]) - timestamp(r[first_recovered - 1]) >= timedelta(seconds=1)
    assert timestamp(h[0]) < min(timestamp(d[-1]), timestamp(r[first_recovered - 1]))
    assert timestamp(h[-1]) > max(timestamp(d[-1]), timestamp(r[first_recovered]))
    for name, report in (("healthy", healthy), ("recovered", recovered), ("disk", disk)):
        print(f"{name}: {report['written']} rows, {len(report['attempts'])} attempts, "
              f"{report['timeouts']} timeouts, {report['sequence_gaps']} gaps, "
              f"{report['buffer_overruns']} overruns", flush=True)


def replacement(exe, root):
    with ExitStack() as stack:
        fixture = adapter(stack, root, "replacement", "127.0.0.4", "replacement")
        run = capture(stack, exe, root, "replacement", "127.0.0.4", 5, 1)
        report = receipt(run, root, "replacement", 1)
        initial, changed = fixture_report(fixture)["attempts"]
    assert initial["serial_number"] != changed["serial_number"]
    assert initial["forward_close"] and "ot_requested_us" not in changed
    first, rejected = report["attempts"]
    assert first["state"] == "Timed out" and rejected["state"] == "Error"
    assert "Identity mismatch" in rejected["identity_error"] and rejected["accepted"] == 0
    assert report["written"] == first["accepted"] > 0
    capture_rows = list(rows(parts_for(Path(report["csv_prefix"]))))
    last_input = int.from_bytes(bytes.fromhex(report["last_input_hex"]), "little")
    assert len(capture_rows) == report["written"] and last_input == int(capture_rows[-1][6])
    print("Same-model replacement rejected before Forward_Open", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    root = args.directory.resolve()
    root.mkdir(parents=True)
    concurrent(args.exe.resolve(), root)
    replacement(args.exe.resolve(), root)


if __name__ == "__main__":
    main()
