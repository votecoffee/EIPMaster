"""Exercise the console scanner against the independent localhost fixture."""

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tomllib

from join_capture import METADATA, parts_for, read_header, rows

FIXTURE = Path(__file__).with_name("loopback_adapter.py")
PROFILE = FIXTURE.parent.parent / "samples" / "loopback.toml"


def with_adapter(root, case, command):
    report = root / f"{case}-adapter.json"
    adapter = subprocess.Popen(
        [sys.executable, FIXTURE, report], stdout=subprocess.PIPE, text=True
    )
    try:
        if adapter.stdout.readline().strip() != "Ready on loopback":
            raise RuntimeError("Loopback adapter failed to start")
        result = subprocess.run(command, capture_output=True, text=True, timeout=15)
        if adapter.wait(timeout=5) != 0:
            raise RuntimeError("Loopback adapter failed")
        return result, json.loads(report.read_text(encoding="utf-8"))
    finally:
        if adapter.poll() is None:
            adapter.terminate()
            adapter.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--rpi-us", type=int, choices=(1000, 8000), default=8000)
    args = parser.parse_args()
    exe = args.exe.resolve()
    root = args.directory.resolve()
    root.mkdir(parents=True)

    imported = subprocess.run([exe, "eds", PROFILE.with_suffix(".eds")],
                              capture_output=True, text=True, timeout=5)
    assert imported.returncode == 0, imported.stderr
    [profile] = tomllib.loads(imported.stdout)["profiles"]
    assert profile["supported"] and profile["id"] == "Connection1"
    assert profile["fields"]["T->O size"] == "4 bytes (Assem101)"

    identity, adapter = with_adapter(root, "identity", [exe, "identity", "127.0.0.1", "127.0.0.1"])
    assert identity.returncode == 0, identity.stderr
    assert tomllib.loads(identity.stdout)["identity"]["name"] == "Loopback Adapter"
    assert "ot_requested_us" not in adapter

    def test_profile(path):
        return path.read_text(encoding="utf-8").replace("rpi_us = 1000", f"rpi_us = {args.rpi_us}")

    legacy = root / "legacy.toml"
    legacy.write_text(test_profile(PROFILE), encoding="utf-8")
    capture = [exe, "capture", legacy]
    mismatch, adapter = with_adapter(root, "mismatch", [*capture, root / "mismatch", "1", "0:12:99"])
    assert mismatch.returncode == 1
    assert "Identity mismatch" in tomllib.loads(mismatch.stdout)["identity_error"]
    assert "ot_requested_us" not in adapter
    assert "last_input_hex" not in tomllib.loads(mismatch.stdout)

    success, adapter = with_adapter(root, "capture", [*capture, root / "capture", "2", "0:12:1"])
    assert success.returncode == 0, (success.stdout, success.stderr)
    report = tomllib.loads(success.stdout)
    assert report == tomllib.loads((root / "capture/capture.toml").read_text(encoding="utf-8"))
    assert adapter["forward_close"] and adapter["ot_invalid"] == 0
    legacy_parts = parts_for(Path(report["csv_prefix"]))
    assert read_header(legacy_parts[0]) == METADATA + ["value"]
    samples = list(rows(legacy_parts))
    assert len(samples) == report["written"] == report["accepted"] > 100
    assert [int(row[2]) for row in samples] == list(range(len(samples)))
    assert all(int(row[6]) == int(row[4]) // 16 for row in samples)
    assert int.from_bytes(bytes.fromhex(report["last_input_hex"]), "little") == int(samples[-1][6])
    assert report["duration_reached"] and report["buffer_overruns"] == 0
    assert report["actual_ot_us"] == report["actual_to_us"] == args.rpi_us

    profile = root / "three-fields.toml"
    fields = [
        ("whole", 0, "U32", False),
        ("first_word", 0, "U16", True),
        ("high_byte", 3, "U8", False),
    ]
    text = test_profile(PROFILE)
    for name, offset, kind, big_endian in fields:
        text += (f'\n[[capture_fields]]\nname = "{name}"\n[capture_fields.selection]\n'
                 f'offset = {offset}\nkind = "{kind}"\nbig_endian = {str(big_endian).lower()}\n')
    profile.write_text(text, encoding="utf-8")
    multi, adapter = with_adapter(
        root, "three-fields", [exe, "capture", profile, root / "three-fields", "2", "0:12:1"]
    )
    assert multi.returncode == 0, (multi.stdout, multi.stderr)
    multi_report = tomllib.loads(multi.stdout)
    parts = parts_for(Path(multi_report["csv_prefix"]))
    header = read_header(parts[0])
    assert header == METADATA + [name for name, *_ in fields]
    multi_samples = list(rows(parts))
    assert len(multi_samples) == multi_report["written"] > 100 and adapter["forward_close"]
    for row in multi_samples:
        expected = int(row[4]) // 16
        payload = expected.to_bytes(4, "little")
        assert list(map(int, row[6:])) == [expected, int.from_bytes(payload[:2], "big"), payload[3]]

    mapped = root / "mapped.toml"
    mapped.write_text(test_profile(PROFILE.with_name("mapped-loopback.toml"))
                      + '\n[app]\ncapacity = 512\nrotate_seconds = 1\nschedule = "tokio"\n',
                      encoding="utf-8")
    filtered, adapter = with_adapter(
        root, "on-change", [exe, "capture", mapped, root / "on-change", "3", "0:12:1"]
    )
    assert filtered.returncode == 0, (filtered.stdout, filtered.stderr)
    filtered_report = tomllib.loads(filtered.stdout)
    parts = parts_for(Path(filtered_report["csv_prefix"]))
    assert filtered_report["schedule"] == "tokio" and len(parts) >= 2
    assert read_header(parts[0]) == METADATA + ["counter", "low_bit"]
    changed = list(rows(parts))
    assert len(changed) == filtered_report["written"] > 10 and adapter["forward_close"]
    assert filtered_report["unchanged"] > len(changed)
    assert filtered_report["written"] + filtered_report["unchanged"] == filtered_report["accepted"]
    assert [int(row[2]) for row in changed] == list(range(len(changed)))
    for i, row in enumerate(changed):
        value = int(row[4]) // 16
        assert int(row[6]) == value
        assert row[7] == ("true" if value & 1 else "false")
        if i:
            assert row[6:] != changed[i - 1][6:]

    invalid = root / "invalid-fields.toml"
    invalid.write_text(text.replace('name = "high_byte"', 'name = "packet_sequence"'), encoding="utf-8")
    bad = subprocess.run([exe, "capture", invalid, root / "invalid", "1", "0:12:1"],
                         capture_output=True, text=True, timeout=5)
    assert bad.returncode == 1 and "Duplicate capture field name" in bad.stderr
    assert not (root / "invalid").exists()

    unsafe = root / "output-owner.toml"
    unsafe.write_text(PROFILE.read_text(encoding="utf-8").replace("input_only = true", "input_only = false"), encoding="utf-8")
    reject = subprocess.run([exe, "capture", unsafe, root / "unsafe", "1", "0:12:1"],
                            capture_output=True, text=True, timeout=5)
    assert reject.returncode == 1 and "input-only" in reject.stderr
    assert not (root / "unsafe").exists()
    print(f"CLI verified: EDS, identity, mismatch rejected, {len(samples)} legacy rows, "
          f"{len(multi_samples)} multi-field rows, {len(changed)} change rows, unsafe profile rejected.")


if __name__ == "__main__":
    main()
