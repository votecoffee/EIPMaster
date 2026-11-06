"""Exercise the console scanner against the independent localhost fixture."""

import argparse
from itertools import pairwise
import json
from math import isclose
from pathlib import Path
import subprocess
import sys
import tomllib

from join_capture import METADATA, parts_for, read_header, rows

FIXTURE = Path(__file__).with_name("loopback_adapter.py")
PROFILE = FIXTURE.parent.parent / "samples" / "loopback.toml"
NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)


def with_adapter(root, case, command, mode="normal"):
    report = root / f"{case}-adapter.json"
    adapter = subprocess.Popen(
        [sys.executable, FIXTURE, report, "--mode", mode], stdout=subprocess.PIPE, text=True,
        creationflags=NO_WINDOW
    )
    try:
        if adapter.stdout.readline().strip() != "Ready on loopback":
            raise RuntimeError("Loopback adapter failed to start")
        result = subprocess.run(command, capture_output=True, text=True, timeout=15, creationflags=NO_WINDOW)
        if adapter.wait(timeout=5) != 0:
            raise RuntimeError("Loopback adapter failed")
        return result, json.loads(report.read_text(encoding="utf-8"))
    finally:
        if adapter.poll() is None:
            adapter.terminate()
            adapter.wait()


def check_spacing(capture, samples):
    receive = capture["receive_spacing"]
    send = capture["send_spacing"]
    assert receive["intervals"] == capture["accepted"] - 1
    assert send["intervals"] == capture["sent"] - 1
    timestamps = [int(row[1]) for row in samples]
    assert abs(receive["span_us"] - (timestamps[-1] - timestamps[0])) <= 1
    for spacing, rpi in [(receive, capture["actual_to_us"]), (send, capture["actual_ot_us"])]:
        span = spacing["span_us"]
        rate = spacing["observed_packets_per_second"]
        assert isclose(rate, spacing["intervals"] * 1e6 / span, rel_tol=1 / span + 1e-12)
        assert isclose(spacing["percent_of_negotiated_rate"], 100 * rate / (1e6 / rpi))
    intervals = [end - start for start, end in pairwise(timestamps)]
    for multiple in (2, 4):
        key = f"intervals_at_or_over_{multiple}x_rpi"
        threshold = multiple * capture["actual_to_us"]
        # CSV truncates each elapsed timestamp separately. A difference equal
        # to the threshold may be on either side before truncation.
        assert sum(gap > threshold for gap in intervals) <= receive[key] <= sum(
            gap >= threshold for gap in intervals
        )
        assert 0 <= send[key] <= send["intervals"]
        assert bool(send[key]) == (send["max_us"] >= multiple * capture["actual_ot_us"])


def check_field_summaries(report, names, samples):
    summaries = report["field_summaries"]
    assert [field["name"] for field in summaries] == names
    for column, summary in enumerate(summaries, len(METADATA)):
        values = [row[column] for row in samples]
        assert summary["first_value"] == values[0] and summary["last_value"] == values[-1]
        assert summary["changes_within_generation"] == sum(
            left[3] == right[3] and left[column] != right[column]
            for left, right in pairwise(samples)
        )
        if summary["selection"]["kind"] == "Bool":
            assert "minimum" not in summary and "maximum" not in summary
        else:
            numbers = [int(value) for value in values]
            assert summary["minimum"] == str(min(numbers))
            assert summary["maximum"] == str(max(numbers))


def check_timeout_multiplier(exe, root, legacy):
    for code, factor in [(3, 32), (4, 64)]:
        name = f"timeout-code-{code}"
        profile = root / f"{name}.toml"
        profile.write_text(f"timeout_multiplier_code = {code}\n"
                           + legacy.read_text(encoding="utf-8"), encoding="utf-8")
        result, fixture = with_adapter(
            root, name, [exe, "capture", profile, root / name, "1", "0:12:1"],
            "pause-different-api",
        )
        report = tomllib.loads(result.stdout)
        assert fixture["timeout_multiplier_code"] == code and fixture["pause_after_sequence"] == 75
        assert report["actual_ot_us"] == 1250 and report["actual_to_us"] == 1500
        assert report["connection_timeout"] == {
            "multiplier_code": code, "factor": factor,
            "requested_receive_watchdog_us": report["requested_to_us"] * factor,
            "receive_watchdog_us": 1500 * factor,
        }
        assert fixture["forward_close"] and report["forward_close"] == {"outcome": "confirmed"}
        assert report["accepted"] == report["written"] > 0 and report["buffer_overruns"] == 0
        if code == 3:
            assert result.returncode == 1 and report["state"] == "Timed out"
            assert report["receive_timeout"]["limit_us"] == 48_000
        else:
            assert result.returncode == 0 and report["state"] == "Closed"
            assert report["receive_spacing"]["max_us"] >= 55_000 and report["sequence_gaps"] == 0
    print("Timeout code 3 expired at 48 ms and code 4 completed with a 96 ms watchdog")


def check_close_failures(capture_command, root):
    for mode in ("close-reject", "close-mismatch", "close-short", "close-eof", "stall-close-reject"):
        out = root / mode
        stalled = mode.startswith("stall-")
        result, adapter = with_adapter(root, mode, [*capture_command, out, "2", "0:12:1"], mode)
        assert result.returncode == 1 and adapter["forward_close"]
        report = tomllib.loads(result.stdout)
        assert report == tomllib.loads((out / "capture.toml").read_text(encoding="utf-8"))
        assert report["state"] == ("Timed out" if stalled else "Close unconfirmed")
        assert report["duration_reached"] == (not stalled)
        assert report["timeouts"] == int(stalled)
        assert ("receive_timeout" in report) == stalled
        assert "cip_error" not in report and "network_error" not in report
        close = report["forward_close"]
        if mode.endswith("reject"):
            assert close == {"outcome": "rejected", "general_status": 1, "extended_status": [0x0107]}
        else:
            assert close["outcome"] == "unconfirmed" and close["error"]
            if mode == "close-mismatch":
                assert "Mismatched Forward_Close connection identifiers" in close["error"]
            if mode == "close-short":
                assert "short reply" in close["error"]
        parts = parts_for(Path(report["csv_prefix"]))
        samples = list(rows(parts))
        assert len(samples) == report["written"] == report["accepted"] > 0
    print("Forward_Close checks: rejection, mismatched IDs, short reply, missing reply, and watchdog preserved")


def check_qualification(exe, root):
    profile = root / "source.toml"
    source = ('capture_mode = "on_change"\n' + PROFILE.read_text(encoding="utf-8")
              + "\n[app]\nauto_reconnect = true\n")
    profile.write_text(source, encoding="utf-8")
    serial = 0x12345678
    command = [exe, "qualify", profile]

    for case, mode in [("qualified", "qualify"), ("interval-mismatch", "qualify-different-api")]:
        out = root / case
        result, fixture = with_adapter(
            root, case, [*command, out, "loopback", f"0:12:1:{serial}", "1"], mode
        )
        receipt = tomllib.loads(result.stdout)
        assert receipt == tomllib.loads((out / "qualification.toml").read_text(encoding="utf-8"))
        assert receipt["mode"] == "headless" and receipt["link_path"] == "loopback"
        assert receipt["expected_identity"] == f"0:12:1:{serial}"
        assert receipt["baseline_seconds"] == receipt["target_seconds"] == 1
        assert receipt["finished_utc"] >= receipt["started_utc"] and "current_run" not in receipt
        assert [trial["name"] for trial in receipt["runs"]] == receipt["planned_runs"] == [
            "baseline-8ms", "trial-1", "trial-2", "trial-3",
        ]
        assert result.returncode == int(any(trial["errors"] for trial in receipt["runs"]))
        assert receipt["outcome"] == ("passed" if result.returncode == 0 else "failed")
        for expected_rpi, name in [(8000, "baseline-profile.toml"), (1000, "target-profile.toml")]:
            effective = tomllib.loads((out / name).read_text(encoding="utf-8"))
            assert effective["input_rpi_us"] == effective["output_rpi_us"] == expected_rpi
            assert effective["capture_mode"] == "every_packet"
            assert not effective["app"].get("auto_reconnect", False)
        for index, (trial, adapter) in enumerate(zip(receipt["runs"], fixture["attempts"], strict=True)):
            capture = trial["capture"]
            assert capture == tomllib.loads((out / trial["name"] / "capture.toml").read_text(encoding="utf-8"))
            assert capture["identity"]["serial"] == serial and adapter["forward_close"]
            rpi = 8000 if index == 0 else 1000
            assert capture["requested_ot_us"] == capture["requested_to_us"] == rpi
            assert adapter["ot_requested_us"] == adapter["to_requested_us"] == rpi
            assert capture["state"] == "Closed" and capture["duration_reached"]
            assert capture["forward_close"] == {"outcome": "confirmed"}
            assert capture["written"] == capture["accepted"] == capture["seen"] > 50
            assert capture["unchanged"] == 0 and "attempts" not in capture
            parts = parts_for(Path(capture["csv_prefix"]))
            samples = list(rows(parts))
            assert [int(row[2]) for row in samples] == list(range(capture["written"]))
            assert all(int(row[6]) == int(row[4]) // 16 for row in samples)
            check_spacing(capture, samples)
            check_field_summaries(capture, ["value"], samples)
            if case == "interval-mismatch" and index == 1:
                assert (capture["actual_ot_us"], capture["actual_to_us"]) == (1250, 1500)
                assert any("Forward_Open intervals differ" in error for error in trial["errors"])
            else:
                assert capture["actual_ot_us"] == capture["actual_to_us"] == rpi
        if case == "interval-mismatch":
            assert receipt["outcome"] == "failed"
        print(f"{case}: {receipt['outcome']}, four captures with persisted receipts")

    out = root / "wrong-serial"
    result, adapter = with_adapter(
        root, "wrong-serial", [*command, out, "loopback", f"0:12:1:{serial + 1}", "1"]
    )
    receipt = tomllib.loads(result.stdout)
    assert result.returncode == 1 and receipt["outcome"] == "failed"
    assert len(receipt["runs"]) == 1 and not (out / "trial-1").exists()
    capture = receipt["runs"][0]["capture"]
    assert "Identity mismatch" in capture["identity_error"] and capture["accepted"] == 0
    assert "ot_requested_us" not in adapter

    out = root / "target-replacement"
    result, fixture = with_adapter(
        root, "target-replacement", [*command, out, "loopback", f"0:12:1:{serial}", "1"],
        "qualify-replacement",
    )
    receipt = tomllib.loads(result.stdout)
    assert result.returncode == 1 and receipt["outcome"] == "failed"
    assert [run["name"] for run in receipt["runs"]] == ["baseline-8ms", "trial-1"]
    assert not (out / "trial-2").exists()
    assert receipt["runs"][0]["capture"]["duration_reached"]
    assert "Identity mismatch" in receipt["runs"][1]["capture"]["identity_error"]
    original, replacement = fixture["attempts"]
    assert original["forward_close"] and original["serial_number"] != replacement["serial_number"]
    assert "ot_requested_us" not in replacement

    out = root / "baseline-mismatch"
    result, adapter = with_adapter(
        root, "baseline-mismatch", [*command, out, "loopback", f"0:12:1:{serial}", "1"],
        "different-api",
    )
    receipt = tomllib.loads(result.stdout)
    assert result.returncode == 1 and receipt["outcome"] == "failed"
    assert len(receipt["runs"]) == 1 and not (out / "trial-1").exists()
    assert adapter["forward_close"] and any("Forward_Open intervals differ" in error
                                            for error in receipt["runs"][0]["errors"])

    out = root / "target-close-rejected"
    result, fixture = with_adapter(
        root, "target-close-rejected", [*command, out, "loopback", f"0:12:1:{serial}", "1"],
        "qualify-close-reject",
    )
    receipt = tomllib.loads(result.stdout)
    assert result.returncode == 1 and receipt["outcome"] == "failed"
    assert [run["name"] for run in receipt["runs"]] == ["baseline-8ms", "trial-1"]
    assert not (out / "trial-2").exists()
    assert all(attempt["forward_close"] for attempt in fixture["attempts"])
    target = receipt["runs"][1]
    assert any("Forward_Close rejected" in error for error in target["errors"])
    assert target["capture"]["forward_close"]["extended_status"] == [0x0107]
    assert target["capture"]["accepted"] == target["capture"]["written"] > 0

    for name, args, expected_error in [
        ("wrong-path", ["direct", f"0:12:1:{serial}"], "non-loopback addresses"),
        ("missing-serial", ["loopback", "0:12:1", "1"], "requires decimal vendor:type:product:serial"),
    ]:
        out = root / name
        result = subprocess.run([*command, out, *args], capture_output=True, text=True,
                                timeout=5, creationflags=NO_WINDOW)
        assert result.returncode == 1 and expected_error in result.stderr and not out.exists()
    unsafe = root / "configuration-write.toml"
    unsafe.write_text(source.replace("config_size = 0", "config_size = 1").replace('config_hex = ""', 'config_hex = "00"'),
                      encoding="utf-8")
    out = root / "configuration-write"
    result = subprocess.run([exe, "qualify", unsafe, out, "loopback", f"0:12:1:{serial}", "1"],
                            capture_output=True, text=True, timeout=5, creationflags=NO_WINDOW)
    assert result.returncode == 1 and "zero configuration bytes" in result.stderr and not out.exists()
    assert profile.read_text(encoding="utf-8") == source
    print("Qualification stops after an unconfirmed close and rejects serial changes, interval mismatches, link labels, and writes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--rpi-us", type=int, choices=(1000, 8000), default=8000)
    parser.add_argument("--qualification", action="store_true", help="Exercise the four-run headless bench command")
    args = parser.parse_args()
    exe = args.exe.resolve()
    root = args.directory.resolve()
    root.mkdir(parents=True)
    if args.qualification:
        check_qualification(exe, root)
        return

    imported = subprocess.run([exe, "eds", PROFILE.with_suffix(".eds")],
                              capture_output=True, text=True, timeout=5)
    assert imported.returncode == 0, imported.stderr
    [profile] = tomllib.loads(imported.stdout)["profiles"]
    assert profile["supported"] and profile["id"] == "Connection1"
    assert profile["fields"]["T->O size"] == "4 bytes (Assem101)"
    assert profile["assembly_definition_sizes"] == ["T->O size"]

    validate = [exe, "validate", PROFILE]
    checked = subprocess.run([*validate, PROFILE.with_suffix(".eds"), "Connection1"],
                             capture_output=True, text=True, timeout=5)
    assert checked.returncode == 0, checked.stderr
    preflight = tomllib.loads(checked.stdout)
    assert preflight["connection_valid"] and preflight["capture_config_valid"]
    assert not preflight["requests_output_ownership"] and preflight["configuration_bytes"] == 0
    fields = {item["field"]: item for item in preflight["origins"]}
    assert fields["T->O size"]["state"] == "assembly_definition"
    assert fields["T->O size"]["eds_size_is_layout"]
    assert fields["Timeout multiplier code"]["state"] == "manual"
    assert preflight["connection_timeout"] == {
        "multiplier_code": 3, "factor": 32, "requested_receive_watchdog_us": 32_000,
    }

    identity, adapter = with_adapter(root, "identity", [exe, "identity", "127.0.0.1", "127.0.0.1"])
    assert identity.returncode == 0, identity.stderr
    assert tomllib.loads(identity.stdout)["identity"]["name"] == "Loopback Adapter"
    assert "ot_requested_us" not in adapter

    def test_profile(path):
        return path.read_text(encoding="utf-8").replace("rpi_us = 1000", f"rpi_us = {args.rpi_us}")

    legacy = root / "legacy.toml"
    legacy.write_text(test_profile(PROFILE), encoding="utf-8")
    invalid_timeout = root / "invalid-timeout.toml"
    invalid_timeout.write_text("timeout_multiplier_code = 8\n" + legacy.read_text(encoding="utf-8"),
                               encoding="utf-8")
    invalid = subprocess.run([exe, "validate", invalid_timeout], capture_output=True, text=True,
                             timeout=5, creationflags=NO_WINDOW)
    invalid_report = tomllib.loads(invalid.stdout)
    assert invalid.returncode == 1 and not invalid_report["connection_valid"]
    assert invalid_report["connection_timeout"] == {"multiplier_code": 8}
    assert any("CIP timeout multiplier code" in error for error in invalid_report["connection_errors"])
    size_override = root / "size-override.toml"
    size_override.write_text(test_profile(PROFILE).replace("input_size = 4", "input_size = 8"), encoding="utf-8")
    checked = subprocess.run([exe, "validate", size_override, PROFILE.with_suffix(".eds"), "Connection1"],
                             capture_output=True, text=True, timeout=5)
    assert checked.returncode == 0, checked.stderr
    fields = {item["field"]: item for item in tomllib.loads(checked.stdout)["origins"]}
    assert fields["T->O size"]["state"] == "override"
    assert fields["T->O size"]["effective"] == "8" and fields["T->O size"]["eds_size_is_layout"]

    unsupported_eds = root / "unsupported.eds"
    unsupported_eds.write_text(PROFILE.with_suffix(".eds").read_text(encoding="utf-8")
                               .replace("0x22440305", "0x44244305"), encoding="utf-8")
    blocked = subprocess.run([*validate, unsupported_eds, "Connection1"],
                             capture_output=True, text=True, timeout=5)
    assert blocked.returncode == 1 and not tomllib.loads(blocked.stdout)["connection_valid"]
    capture = [exe, "capture", legacy]
    mismatch, adapter = with_adapter(root, "mismatch", [*capture, root / "mismatch", "1", "0:12:99"])
    assert mismatch.returncode == 1
    assert "Identity mismatch" in tomllib.loads(mismatch.stdout)["identity_error"]
    assert "ot_requested_us" not in adapter
    assert "last_input_hex" not in tomllib.loads(mismatch.stdout)
    assert "forward_close" not in tomllib.loads(mismatch.stdout)

    for mode in ("no-input", "invalid-input"):
        no_input, adapter = with_adapter(
            root, mode, [*capture, root / mode, "1", "0:12:1"], mode
        )
        report = tomllib.loads(no_input.stdout)
        assert no_input.returncode == 1 and report["state"] == "Timed out" and report["timeouts"] == 1
        assert report["accepted"] == report["written"] == 0 and adapter["forward_close"]
        timeout = report["receive_timeout"]
        assert timeout["waiting_for_first_input"] and timeout["limit_us"] == 2_000_000
        assert timeout["valid_input_silence_us"] >= timeout["limit_us"]
        assert "last_sequence" not in timeout
        if mode == "invalid-input":
            assert adapter["to_sent"] >= report["invalid"] > 0
            assert timeout["last_udp_age_us"] < timeout["valid_input_silence_us"]
        else:
            assert report["invalid"] == adapter["to_sent"] == 0 and "last_udp_age_us" not in timeout

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
    assert adapter["timeout_multiplier_code"] == report["connection_timeout"]["multiplier_code"] == 3
    assert report["forward_close"] == {"outcome": "confirmed"}
    assert report["actual_ot_us"] == report["actual_to_us"] == args.rpi_us
    check_spacing(report, samples)
    timing = report["send_timing"]
    assert "above_normal_applied" not in timing
    assert timing["max_dispatch_lateness_us"] >= (args.rpi_us if report["skipped_deadlines"] else 0)
    assert timing["max_send_call_us"] >= (args.rpi_us if timing["send_calls_at_or_over_rpi"] else 0)
    check_close_failures(capture, root)
    check_timeout_multiplier(exe, root, legacy)

    slow, adapter = with_adapter(
        root, "slow-input", [*capture, root / "slow-input", "2", "0:12:1"], "slow-input"
    )
    assert slow.returncode == 0 and adapter["forward_close"], (slow.stdout, slow.stderr)
    slow_report = tomllib.loads(slow.stdout)
    assert slow_report["actual_to_us"] == args.rpi_us
    assert slow_report["sequence_gaps"] == slow_report["buffer_overruns"] == 0
    assert slow_report["written"] == slow_report["accepted"]
    slow_spacing = slow_report["receive_spacing"]
    assert 35 < slow_spacing["percent_of_negotiated_rate"] < 55
    slow_samples = list(rows(parts_for(Path(slow_report["csv_prefix"]))))
    check_spacing(slow_report, slow_samples)
    print(f"Contiguous input at {slow_spacing['percent_of_negotiated_rate']:.1f}% of negotiated rate")

    if sys.platform == "win32":
        priority = root / "priority.toml"
        priority.write_text(test_profile(PROFILE) + '\n[app]\nschedule = "deadline_above_normal"\n',
                            encoding="utf-8")
        result, adapter = with_adapter(
            root, "priority", [exe, "capture", priority, root / "priority", "1", "0:12:1"]
        )
        assert result.returncode == 0, (result.stdout, result.stderr)
        receipt = tomllib.loads(result.stdout)
        assert receipt["schedule"] == "deadline_above_normal" and adapter["forward_close"]
        timing = receipt["send_timing"]
        assert isinstance(timing["above_normal_applied"], bool)
        assert timing["max_dispatch_lateness_us"] >= 0 and timing["max_send_call_us"] >= 0

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
    check_field_summaries(multi_report, [name for name, *_ in fields], multi_samples)

    mapped = root / "mapped.toml"
    mapped.write_text(test_profile(PROFILE.with_name("mapped-loopback.toml"))
                      + '\n[app]\ncapacity = 512\nrotate_seconds = 1\ngui_capture_seconds = 1\nschedule = "tokio"\n',
                      encoding="utf-8")
    filtered, adapter = with_adapter(
        root, "on-change", [exe, "capture", mapped, root / "on-change", "3", "0:12:1"]
    )
    assert filtered.returncode == 0, (filtered.stdout, filtered.stderr)
    filtered_report = tomllib.loads(filtered.stdout)
    assert filtered_report["duration_seconds"] == 3 and filtered_report["duration_reached"]
    assert filtered_report["receive_spacing"]["span_us"] > 2_000_000
    parts = parts_for(Path(filtered_report["csv_prefix"]))
    assert filtered_report["schedule"] == "tokio" and len(parts) >= 2
    timing = filtered_report["send_timing"]
    assert "max_dispatch_lateness_us" not in timing and timing["max_send_call_us"] >= 0
    assert read_header(parts[0]) == METADATA + ["counter", "low_bit"]
    changed = list(rows(parts))
    assert len(changed) == filtered_report["written"] > 10 and adapter["forward_close"]
    assert filtered_report["unchanged"] > len(changed)
    assert filtered_report["written"] + filtered_report["unchanged"] == filtered_report["accepted"]
    assert [int(row[2]) for row in changed] == list(range(len(changed)))
    check_field_summaries(filtered_report, ["counter", "low_bit"], changed)
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
    owner_check = subprocess.run([exe, "validate", unsafe], capture_output=True, text=True, timeout=5)
    assert owner_check.returncode == 0 and tomllib.loads(owner_check.stdout)["requests_output_ownership"]
    reject = subprocess.run([exe, "capture", unsafe, root / "unsafe", "1", "0:12:1"],
                            capture_output=True, text=True, timeout=5)
    assert reject.returncode == 1 and "input-only" in reject.stderr
    assert not (root / "unsafe").exists()
    print(f"CLI verified: EDS, preflight, identity, first-input timeout diagnostics, {len(samples)} legacy rows, "
          f"{len(multi_samples)} multi-field rows, {len(changed)} change rows, unsafe profile rejected.")


if __name__ == "__main__":
    main()
