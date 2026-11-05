"""Exercise the console scanner against the independent localhost fixture."""

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tomllib

from join_capture import parts_for, rows

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
    args = parser.parse_args()
    exe = args.exe.resolve()
    root = args.directory.resolve()
    root.mkdir(parents=True)

    identity, adapter = with_adapter(root, "identity", [exe, "identity", "127.0.0.1", "127.0.0.1"])
    assert identity.returncode == 0, identity.stderr
    assert tomllib.loads(identity.stdout)["identity"]["name"] == "Loopback Adapter"
    assert "ot_requested_us" not in adapter

    capture = [exe, "capture", PROFILE]
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
    samples = list(rows(parts_for(Path(report["csv_prefix"]))))
    assert len(samples) == report["written"] == report["accepted"] > 100
    assert [int(row[2]) for row in samples] == list(range(len(samples)))
    assert all(int(row[6]) == int(row[4]) // 16 for row in samples)
    assert int.from_bytes(bytes.fromhex(report["last_input_hex"]), "little") == int(samples[-1][6])
    assert report["duration_reached"] and report["buffer_overruns"] == 0
    assert report["actual_ot_us"] == report["actual_to_us"] == 1000

    unsafe = root / "output-owner.toml"
    unsafe.write_text(PROFILE.read_text(encoding="utf-8").replace("input_only = true", "input_only = false"), encoding="utf-8")
    reject = subprocess.run([exe, "capture", unsafe, root / "unsafe", "1", "0:12:1"],
                            capture_output=True, text=True, timeout=5)
    assert reject.returncode == 1 and "input-only" in reject.stderr
    assert not (root / "unsafe").exists()
    print(f"CLI verified: identity, identity mismatch before Forward_Open, {len(samples)} CSV rows, output ownership rejected.")


if __name__ == "__main__":
    main()
