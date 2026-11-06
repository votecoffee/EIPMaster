"""Build the pinned OpENer sample adapter and test Class 1 input-only capture on Windows."""
import argparse
from contextlib import contextmanager
from io import BytesIO
from pathlib import Path
import socket
import subprocess
import sys
import time
import tomllib
import zipfile

from join_capture import METADATA, parts_for, rows

HERE = Path(__file__).resolve().parent
UPSTREAM = "0e1ab7ee5c272950c0dc8daf7793008d7a518912"
ADAPTER = "127.0.0.2"
PROFILE = HERE.parent / "samples" / "opener-loopback.toml"
READY = "OpENer loopback ready"


def build_adapter(checkout, root):
    archive = subprocess.check_output(
        ["git", "-C", checkout, "archive", "--format=zip", UPSTREAM],
        creationflags=subprocess.CREATE_NO_WINDOW,
    )
    source = root / "upstream"
    with zipfile.ZipFile(BytesIO(archive)) as bundle:
        bundle.extractall(source)
    ports = source / "source/src/ports"
    main = HERE / "fixtures/opener_loopback.c"
    (ports / "WIN32/main.c").write_bytes(main.read_bytes())

    handler = ports / "generic_networkhandler.c"
    data = handler.read_bytes()
    wildcard = b".sin_addr.s_addr = htonl(INADDR_ANY)"
    if data.count(wildcard) != 2:
        raise RuntimeError("Review OpENer socket bindings before running the adapter")
    handler.write_bytes(data.replace(wildcard, b".sin_addr.s_addr = g_network_status.ip_address"))

    build = root / "build"
    log = root / "build.log"
    commands = [
        ["cmake", "-S", source / "source", "-B", build, "-G", "Visual Studio 17 2022", "-A", "x64",
         "-DOpENer_PLATFORM=WIN32", "-DCMAKE_C_STANDARD=11", "-DOpENer_TRACES=ON"],
        ["cmake", "--build", build, "--config", "Release", "--parallel", "8"],
    ]
    with log.open("xb") as stream:
        for command in commands:
            proc = subprocess.run(command, stdout=stream, stderr=subprocess.STDOUT,
                                  creationflags=subprocess.CREATE_NO_WINDOW, timeout=180)
            if proc.returncode != 0:
                raise RuntimeError(f"OpENer build failed. See {log}")
    return build / "src/ports/WIN32/Release/OpENer.exe", source / "data/opener_sample_app.eds"


@contextmanager
def run_adapter(exe, root):
    for kind, port in [(socket.SOCK_STREAM, 44818), (socket.SOCK_DGRAM, 44818), (socket.SOCK_DGRAM, 2222)]:
        with socket.socket(socket.AF_INET, kind) as check:
            check.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
            check.bind((ADAPTER, port))
    log = root / "adapter.log"
    with log.open("xb") as stream, subprocess.Popen(
        [exe], cwd=root, stdin=subprocess.PIPE, stdout=stream, stderr=subprocess.STDOUT,
        creationflags=subprocess.CREATE_NO_WINDOW,
    ) as adapter:
        try:
            deadline = time.monotonic() + 5
            while READY not in log.read_text(encoding="utf-8", errors="replace"):
                if adapter.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"OpENer did not start. See {log}")
                time.sleep(0.05)
            yield log
        finally:
            if adapter.poll() is None:
                try:
                    adapter.communicate(b"\n", timeout=5)
                except subprocess.TimeoutExpired:
                    adapter.kill()
                    adapter.wait(timeout=5)
        if adapter.returncode != 0:
            raise RuntimeError(f"OpENer exited with code {adapter.returncode}. See {log}")


def call(cli, root, label, *args, timeout=10):
    result = subprocess.run([cli, *args], capture_output=True, timeout=timeout,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    (root / f"{label}.toml").write_bytes(result.stdout)
    if result.returncode != 0:
        (root / f"{label}.stderr.txt").write_bytes(result.stderr)
        raise RuntimeError(f"{label} exited with code {result.returncode}. See {root}")
    return tomllib.loads(result.stdout.decode("utf-8"))


def check_capture(report, seconds):
    assert report["state"] == "Closed" and report["duration_reached"]
    assert report["forward_close"] == {"outcome": "confirmed"}
    assert report["input_assembly"] == 100 and report["input_size"] == 32
    assert report["last_input_hex"] == bytes(range(32)).hex(" ").upper()
    assert all(report[key] == 30000 for key in (
        "requested_ot_us", "requested_to_us", "actual_ot_us", "actual_to_us",
    ))
    assert not any(report[key] for key in ("timeouts", "invalid", "buffer_overruns"))
    assert "disk_error" not in report
    parts = parts_for(Path(report["csv_prefix"]))
    samples = list(rows(parts, METADATA + ["first_word_le", "last_word_be"]))
    assert len(samples) == report["written"] == report["seen"] >= seconds * 30
    after_deadline = report["after_capture_deadline"]
    assert report["accepted"] == report["seen"] + after_deadline
    assert seconds <= 10 or len(parts) > 1
    assert all((int(row[2]), int(row[3]), int(row[6]), int(row[7])) ==
               (index, 1, 0x03020100, 0x1C1D1E1F) for index, row in enumerate(samples))
    elapsed = [int(row[1]) for row in samples]
    assert elapsed[-1] - elapsed[0] <= seconds * 1_000_000
    gaps = sorted(later - earlier for earlier, later in zip(elapsed, elapsed[1:]))
    receive = report["receive_spacing"]
    assert receive["intervals"] == len(gaps) + after_deadline
    if after_deadline == 0:
        assert abs(receive["max_us"] - gaps[-1]) <= 1
        for pct in (50, 95, 99):
            expected = gaps[(len(gaps) * pct + 99) // 100 - 1]
            # CSV elapsed times are truncated separately. The coarse histogram rounds upward.
            assert expected - 1 <= receive[f"p{pct}_us"] <= expected + expected // 256 + 1
    print(f"{len(samples)} rows in {len(parts)} parts, {report['sequence_gaps']} sequence gaps, "
          f"{report['buffer_overruns']} overruns")
    print(f"Receive spacing (µs) {receive}")
    print(f"Send spacing (µs) {report['send_spacing']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path, help="Path to eipmaster-cli.exe")
    parser.add_argument("checkout", type=Path, help="OpENer Git checkout containing the pinned commit")
    parser.add_argument("directory", type=Path, help="New directory for the adapter build and capture")
    parser.add_argument("--seconds", type=int, default=60)
    args = parser.parse_args()
    if sys.platform != "win32" or not 2 <= args.seconds <= 300:
        parser.error("This fixture requires Windows and a duration from 2 to 300 seconds")
    root = args.directory.resolve()
    root.mkdir(parents=True)
    cli = args.exe.resolve()
    adapter, eds = build_adapter(args.checkout.resolve(), root)
    imported = call(cli, root, "eds", "eds", eds)
    profile = next(p for p in imported["profiles"] if p["id"] == "Connection2")
    assert profile["supported"] and profile["fields"]["Ownership"] == "Input-only"
    checked = call(cli, root, "preflight", "validate", PROFILE, eds, "Connection2")
    assert checked["connection_valid"] and checked["capture_config_valid"]
    assert not checked["requests_output_ownership"] and checked["configuration_bytes"] == 0
    origins = {item["field"]: item["state"] for item in checked["origins"]}
    assert origins.pop("Timeout multiplier code") == "manual"
    assert set(origins.values()) == {"eds"}
    with run_adapter(adapter, root) as log:
        identity = call(cli, root, "identity", "identity", ADAPTER, "127.0.0.1")["identity"]
        assert (identity["vendor"], identity["device_type"], identity["code"], identity["serial"]) == (
            1, 12, 65001, 123456789,
        )
        report = call(cli, root, "result", "capture", PROFILE, root / "capture", str(args.seconds),
                      "1:12:65001", timeout=args.seconds + 15)
    assert report["identity"] == identity
    assert "ForwardClose: ConnSerNo" in log.read_text(encoding="utf-8", errors="replace")
    check_capture(report, args.seconds)


if __name__ == "__main__":
    main()
