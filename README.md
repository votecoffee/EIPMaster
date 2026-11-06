# EIPMaster _(assembly-scanner)_

Windows EtherNet/IP Class 1 scanner and CSV assembly logger.

EIPMaster is the source repository for the Assembly Scanner desktop application. It opens one fixed-size cyclic IPv4 unicast connection, displays device identity and live assembly data, stages typed output values, and records selected inputs to CSV. Each device session owns its TCP and UDP sockets, producer, receiver, output buffer and counters. The CSV writer runs independently.

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
6. Under **Assembly mappings**, define named inputs and outputs. The right-hand **Input assembly** panel shows the live named values and the raw bytes. Save the mapping with the connection settings in a new TOML file.

The initial protocol scope is standard Forward_Open, fixed-size cyclic Class 1 unicast and direct assembly paths. Routed paths, electronic keys, multicast, listen-only, and Large Forward_Open are outside that scope. Keep device-specific presets, vendor files and raw captures in the Git-ignored `.local/` folder. The tracked [sample EDS](samples/loopback.eds) and [sample TOML](samples/loopback.toml) target the localhost fixture.

### Map assembly values

Each field uses a zero-based byte offset and a type. Use offset 6 for the fourth 16-bit word. A single-bit field uses an LSB-first index from 0 to 7 within its byte. **Bool** reads true from any nonzero byte and writes either 0 or 1. A mapped bit write preserves the other bits in the byte. Integer types support signed and unsigned 8-, 16-, 32- and 64-bit values, and floats use IEEE 754 `F32` or `F64`.

Numeric fields can use little or big endian. **Swap bytes within words** exchanges the two bytes of each 16-bit word. **Reverse word order** exchanges the order of the 16-bit words while preserving the byte order inside each word. Reading applies the swap before interpreting endianness. Writing reverses that process. **Utf8** uses a fixed byte length, reads up to the first NUL and pads shorter entries with zeros. Use the vendor's documented format for any length-prefixed string.

For outputs, define **Named output mappings** and open **Stage typed output values**. Start with a full hex buffer or explicitly stage a zero-filled one. Enter a value and select **Stage**. The staged hex and typed readbacks show the same buffer. Connecting as owner sends that buffer immediately after connection. For an open output connection, authorize the staged bytes and select **Apply staged outputs**. Configuration data uses the separate hex field and takes effect on reconnect.

The EDS importer resolves supported connection profiles. Define the application-level value map from the adapter's documentation or load a saved TOML profile. The [mapped loopback profile](samples/mapped-loopback.toml) shows the mapping syntax and selection.

### Capture and join CSV

Start capture before or during a connection. **Log all mapped inputs** includes every named input. Turn it off to choose columns using each field's **CSV** checkbox. **Every packet** writes one row per accepted forward-sequence input packet. **On change** writes the first packet of each connection and subsequent packets when any logged field changes. A written row contains all selected columns. Floats are compared by their IEEE bit patterns and text is compared up to the first NUL.

The timestamp uses the monotonic clock immediately after receive. The CSV includes the UTC capture start, elapsed microseconds, sample index, connection generation and packet sequences. A profile with no named inputs writes the primary selection to the `value` column. The app still accepts older `[[capture_fields]]` tables. New profiles use `[[input_fields]]` and optionally `log_fields` to restrict columns. Omit `log_fields` to include every mapped input. Set capture policy before the connection or capture starts.

```toml
capture_mode = "on_change"
log_fields = ["status_word"]

[[input_fields]]
name = "status_word"

[input_fields.selection]
offset = 0
kind = "U16"
big_endian = false
```

Set either rotation limit to zero to disable it. Each part has the same UTF-8 CSV header. Keep capture armed during a reconnect to retain the UTC origin and sample index. The generation column marks the new connection. Select **Stop capture and flush** to drain the queue.

The live panel separates requested RPIs, Forward_Open actual intervals and socket timing. Capture stats separate intentionally skipped unchanged packets, sequence gaps, duplicates, late and invalid packets, sender deadline skips, local buffer overruns and disk errors. A local overrun leaves a gap in the sample indexes. The queue reserves space up front for packet snapshots. Increasing its sample capacity increases memory use.

### CLI

The console executable uses the same EDS importer, scanner and CSV writer as the GUI. `eds` lists imported profiles, resolved settings and unsupported features offline. `identity` binds to the local IPv4 address and reads the adapter's CIP Identity Object. `capture` checks the expected decimal vendor, device type and product code in the same session before Forward_Open. Use a verified input-only TOML profile with zero output and configuration bytes. The new capture directory's parent must already exist.

```powershell
.\target\release\eipmaster-cli.exe eds samples\loopback.eds
.\target\release\eipmaster-cli.exe identity 127.0.0.1 127.0.0.1
.\target\release\eipmaster-cli.exe capture samples\loopback.toml .\.local\console-capture 10 0:12:1
```

Start `python tools/loopback_adapter.py .local/adapter.json` in another console before each localhost `identity` or `capture` example. Create `.local` first if needed. For a bench run, pass the verified adapter and NIC addresses to `identity` and use the corresponding TOML profile for `capture`. The deadline scheduler runs for the requested number of seconds after the first accepted packet. Results are printed as TOML and saved to `capture.toml` beside the CSV. The report includes the last accepted assembly payload in hex alongside its decoded value. It separates requested and actual intervals, socket spacing, sequence gaps, skipped send slots, unchanged samples and CSV overruns. A failed or incomplete capture returns a nonzero exit code. Review the loss counters after a completed capture.

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
The CLI functional check defaults to an 8 ms RPI. Pass `--rpi-us 1000` to exercise the same cases at 1 ms.

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
