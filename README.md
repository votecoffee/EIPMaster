# EIPMaster _(assembly-scanner)_

Windows EtherNet/IP Class 1 scanner and CSV assembly logger.

EIPMaster is the source repository for the Assembly Scanner desktop application. It opens one fixed-size cyclic IPv4 unicast connection, displays device identity and live assembly data, and records one decoded input value per accepted packet. Each device session owns its TCP and UDP sockets, producer, receiver, output buffer and counters. The CSV writer runs independently.

## Table of Contents

- [Install](#install)
- [Usage](#usage)
- [Verification](#verification)
- [Maintainers](#maintainers)
- [Contributing](#contributing)
- [License](#license)

## Install

### Dependencies

Run the Windows x64 build on Windows with an OpenGL-capable graphics driver. Building from source requires the x64 MSVC Rust toolchain and Visual Studio C++ Build Tools. The loopback and CSV join tools use Python 3.11 or newer.

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo +stable-x86_64-pc-windows-msvc build --locked --release
```

## Usage

Launch the executable from the repository root so the sample paths resolve. For a standalone bundle, launch the executable beside its samples folder.

```powershell
.\target\release\assembly-scanner.exe
```

### Connect to a bench adapter

1. Enter the adapter IPv4 address and the local IPv4 address assigned to the scanner NIC. Choose **Read identity** to verify the target before opening I/O.
2. For EDS setup, enter the vendor EDS path, choose **Import**, and select a connection profile. The importer prefers an available input-only profile. Expand **EDS source values and fields to review** for the imported values and fields needing input. The editable settings below override the imported values. The importer supports literal assembly paths and single-byte bracketed `ParamN` substitutions with declared defaults. It flags incompatible connection masks and unresolved paths.
3. For manual setup, choose **Manual settings** or load a TOML profile. Set the T→O input and O→T output or heartbeat connection points. Use payload sizes excluding transport headers, and confirm the sizes against the adapter. Enter each RPI in microseconds. Port zero requests an ephemeral local UDP port. Enter the selected NIC's fixed UDP port if the adapter requires it.
4. Set the Run/Idle option for each direction. Standard Forward_Open allows up to 509 bytes of assembly payload and optional Run/Idle header per direction after the two-byte Class 1 sequence. Enter the optional configuration instance and its hex bytes when required. A configuration payload is applied in Forward_Open. Review the exact bytes before reconnecting to apply changes.
5. Prefer a supported **Input-only heartbeat profile** for logging. An exclusive-owner connection can take control immediately and sends the staged bytes. An O→T Run/Idle header asserts Run from the first frame. Confirm ownership and the specific output or configuration bytes before checking the authorization box. On an authorized output connection, **Apply staged outputs** swaps the cyclic buffer.
6. Choose the zero-based input byte offset, an optional LSB-first bit, or a numeric type and byte order. The typed view and CSV share the selection. The raw view uses zero-based offsets.

The initial protocol scope is standard Forward_Open, fixed-size cyclic Class 1 unicast and direct assembly paths. Routed paths, electronic keys, multicast, listen-only, and Large Forward_Open are outside that scope. Keep device-specific presets, vendor files and raw captures in the Git-ignored `.local/` folder. The tracked [sample EDS](samples/loopback.eds) and [sample TOML](samples/loopback.toml) target the localhost fixture.

### Capture and join CSV

Start capture before or during a connection. Each accepted forward-sequence input packet contributes a sample even when its value repeats. The timestamp uses the monotonic clock immediately after receive. The CSV includes the UTC capture start, elapsed microseconds, sample index, connection generation and packet sequences.

Set either rotation limit to zero to disable it. Each part has the same UTF-8 CSV header. Keep capture armed during a reconnect to retain the UTC origin and sample index. The generation column marks the new connection. Select **Stop capture and flush** to drain the queue.

The live panel separates requested RPIs, Forward_Open actual intervals and socket timing. Capture stats separate sequence gaps, duplicates, late and invalid packets, sender deadline skips, local buffer overruns and disk errors. A local overrun leaves a gap in the sample indexes.

### CLI

The console executable uses the same scanner and CSV writer as the GUI. `identity` binds to the local IPv4 address and reads the adapter's CIP Identity Object. `capture` checks the expected decimal vendor, device type and product code in the same session before Forward_Open. Use a verified input-only TOML profile with zero output and configuration bytes. The new capture directory's parent must already exist.

```powershell
.\target\release\eipmaster-cli.exe identity 127.0.0.1 127.0.0.1
.\target\release\eipmaster-cli.exe capture samples\loopback.toml .\.local\console-capture 10 0:12:1
```

These examples target the localhost fixture. Before each example, start `python tools/loopback_adapter.py .local/adapter.json` in another console. Create `.local` first if needed. For a bench run, pass the verified adapter and NIC addresses to `identity` and use the corresponding TOML profile for `capture`. The deadline scheduler runs for the requested number of seconds after the first accepted packet. Results are printed as TOML and saved to `capture.toml` beside the CSV. The report includes the last accepted assembly payload in hex alongside its decoded value. It separates requested and actual intervals, socket spacing, sequence gaps, skipped send slots and CSV overruns. A failed or incomplete capture returns a nonzero exit code. Review the loss counters after a completed capture.

Use the capture prefix shown in the GUI to join the parts in order after stopping capture.

```powershell
python tools/join_capture.py 'captures/capture-<UTC-prefix>' joined.csv
```

Run the simulator using the real GUI and a timed CSV capture. Use a new output directory on each run.

```powershell
python tools/benchmark.py .\target\release\assembly-scanner.exe .\.local\loopback-results --full
```

To exercise a custom profile against a localhost fixture, pass its TOML file as the last argument. This mode uses loopback IPv4 for both endpoints and requires an input-only profile without writes.

```powershell
.\target\release\assembly-scanner.exe --loopback-benchmark deadline .\.local\custom-run .\.local\profile.toml
```

## Verification

Run the Rust tests and the loopback harness on Windows. The [verification notes](VERIFICATION.md) describe the upstream crate review, simulator timing and physical acceptance procedure.

```powershell
cargo +stable-x86_64-pc-windows-msvc test --locked --release
python tools/test_cli.py .\target\release\eipmaster-cli.exe .\.local\cli-check
```

## Maintainers

[votecoffee](https://github.com/votecoffee)

## Contributing

Use the [issue tracker](https://github.com/votecoffee/EIPMaster/issues) for reproducible bugs and feature requests. Include the Windows build, the requested and actual intervals, and sanitized logs or a loopback reproduction when reporting a connection issue.

## License

[MIT © votecoffee](LICENSE)
