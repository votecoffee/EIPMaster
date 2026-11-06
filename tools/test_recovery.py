"""Verify per-device recovery and disk failure isolation without opening a GUI."""
import argparse
from contextlib import contextmanager, ExitStack
import csv
from datetime import datetime, timedelta
import json
from pathlib import Path
import shutil
import subprocess
import sys
import time
import tomllib

from join_capture import METADATA, parts_for, read_header, rows

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


def join(prefix, *arguments):
    return subprocess.run(
        [sys.executable, HERE / "join_capture.py", prefix, *arguments],
        capture_output=True, text=True, timeout=10,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )


def check_report_join(root, recovered, parts, captured, disk):
    root.mkdir()
    prefix = Path(recovered["csv_prefix"])
    report = prefix.parent / "capture.toml"
    joined = root / "joined.csv"
    checked = join(prefix, joined, "--report", report)
    assert checked.returncode == 0, checked.stderr
    assert "logged generation count 2" in checked.stdout
    assert list(rows([joined])) == captured
    assert recovered["csv_parts"] == len(parts)
    assert recovered["capture_start_utc"] == captured[0][0]

    moved = root / prefix.name
    for part in parts[:-1]:
        shutil.copyfile(part, root / part.name)
    partial = join(moved, "--report", report)
    assert partial.returncode == 1 and "CSV parts, found" in partial.stderr

    legacy = root / "legacy.toml"
    data = report.read_text(encoding="utf-8")
    legacy.write_text("".join(line for line in data.splitlines(keepends=True)
                              if not line.startswith(("csv_parts = ", "capture_start_utc = "))),
                      encoding="utf-8")
    assert join(prefix, "--report", legacy).returncode == 0
    partial = join(moved, "--report", legacy)
    assert partial.returncode == 1 and "written rows, found" in partial.stderr

    moved_last = root / parts[-1].name
    shutil.copyfile(parts[-1], moved_last)
    assert join(moved, "--report", report).returncode == 0
    header = read_header(parts[-1])
    with moved_last.open("w", encoding="utf-8", newline="") as file:
        csv.writer(file, lineterminator="\r\n").writerow(header)
    bad_output = root / "truncated.csv"
    partial = join(moved, bad_output, "--report", report)
    assert partial.returncode == 1 and "written rows, found" in partial.stderr
    assert not bad_output.exists()
    edited = list(rows([parts[-1]]))
    edited[-1][-1] += "0"
    with moved_last.open("w", encoding="utf-8", newline="") as file:
        writer = csv.writer(file, lineterminator="\r\n")
        writer.writerow(header)
        writer.writerows(edited)
    altered = join(moved, "--report", report)
    assert altered.returncode == 1 and "Last CSV values differ" in altered.stderr

    failed_prefix = Path(disk["csv_prefix"])
    failed = join(failed_prefix, "--report", failed_prefix.parent / "capture.toml")
    assert failed.returncode == 1 and "CSV writer error" in failed.stderr


def check_gui_report_join(root):
    prefix = root / "gui-capture"
    started = "2026-01-01T00:00:00.000000Z"
    saved = [
        [started, "0", "0", "1", "1", "1", "a,\"b\n", "18446744073709551615"],
        [started, "10", "2", "1", "3", "3", "", "000001"],
        [started, "20", "3", "2", "1", "1", "new", "2"],
    ]
    for number, samples in enumerate([saved[:2], saved[2:]], 1):
        path = prefix.with_name(f"{prefix.name}-{number:06}.csv")
        with path.open("x", encoding="utf-8", newline="") as file:
            writer = csv.writer(file, lineterminator="\r\n")
            writer.writerow(METADATA + ["text", "value"])
            writer.writerows(samples)
    body = (f'mode = "gui"\n[capture]\ncsv_prefix = {json.dumps(str(prefix), ensure_ascii=False)}\n'
            f'started_utc = "{started}"\ncsv_parts = 2\nwritten = 3\nunchanged = 1\n')
    report = root / "gui-run-report.toml"
    report.write_text(body + "seen = 6\nbuffer_overruns = 2\n", encoding="utf-8")
    joined = root / "gui-joined.csv"
    checked = join(prefix, joined, "--report", report)
    assert checked.returncode == 0, checked.stderr
    assert "account for 1 of 2 reported overruns" in checked.stdout
    assert "after the last saved row total 1" in checked.stdout
    assert list(rows([joined])) == saved
    old_join = root / "legacy-joined.csv"
    assert join(prefix, old_join).returncode == 0 and list(rows([old_join])) == saved

    wrong = root / "unexplained-gap.toml"
    wrong.write_text(body + "seen = 4\nbuffer_overruns = 0\n", encoding="utf-8")
    checked = join(prefix, "--report", wrong)
    assert checked.returncode == 1 and "missing sample indexes" in checked.stderr
    print("CSV report checks passed for reconnects, missing and truncated parts, and reported overruns")


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
    assert prior["forward_close"] == restored["forward_close"] == {"outcome": "confirmed"}
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
    check_report_join(root / "report-join", recovered, recovered_parts, r, disk)
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
    check_gui_report_join(root)


if __name__ == "__main__":
    main()
