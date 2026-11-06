# EIPMaster _(assembly-scanner)_

Windows EtherNet/IP Class 1 scanner and CSV assembly logger.

EIPMaster is the source repository for the Assembly Scanner desktop application. It opens fixed-size cyclic IPv4 unicast connections, displays device identity and live assembly data, stages typed output values, and records selected inputs to CSV. The GUI can run separate sessions for multiple adapters in one window. Each device session owns its TCP and UDP sockets, producer, receiver, output buffer and counters. The CSV writer runs independently.

## Table of Contents

- [Install](#install)
- [Usage](#usage)
- [Verification](#verification)
- [Roadmap](#roadmap)
- [Maintainers](#maintainers)
- [Contributing](#contributing)
- [License](#license)

## Install

### Portable Windows x64 preview

Extract the preview ZIP and follow the bundled [quick start](QUICKSTART.md). It includes the GUI, CLI, generic loopback examples, and license notices. Use its SHA-256 file to verify the download.

To build the same package from a clean checkout matching `origin/main`, use Python 3.11 or newer and the toolchain below.

```powershell
git fetch origin
python tools/package_windows.py .local/dist
```

The packager builds with locked dependencies and includes the commit, toolchain and file hashes. Use `--offline` with a populated Cargo cache. Use `--toolchain` to select the recorded Rust toolchain when repeating an older build.

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

### Save and restore settings

The Windows GUI automatically remembers the device list and selected device in `%LOCALAPPDATA%\EIPMaster\last-session.toml`. It saves edited settings while the window is open and on a normal close. Each setup includes device and NIC settings, imported EDS profiles and overrides, assembly mappings, staged bytes, and CSV and scheduler preferences. An older single-device session opens as **Device 1**. The app starts offline. Confirm the bench and any output or configuration writes again for each device before connecting.

To keep a named setup, enter a TOML path in **Settings TOML** and select **Save**. Enter a new path to save a separate profile, or select **Replace profile** to update an existing file. Select **Load** to switch to a saved profile while disconnected and with capture stopped. Older profiles still load. The optional `[app]` table stores the capture directory, buffer capacity, rotation limits, scheduler, and automatic reconnect preference.

### Work with multiple adapters

Choose **Add device**, give the setup a name, then enter its settings or load a named TOML profile. Use the **Devices** list to switch between setups while other connections and captures keep running. Each setup has its own identity, connection state, counters, staged bytes, write confirmation, and capture controls. Stop capture and disconnect the selected device before removing its setup.

Use local UDP port 0 when the adapter supports an automatically assigned port. Two simultaneous connections cannot reserve the same fixed local IPv4 address and UDP port. If both adapters require the same fixed port, assign each connection a distinct supported local IPv4 bind address. A fixed port already in use outside EIPMaster can also cause a bind error.

### Recover an input-only session

Select **Read identity**, verify the adapter, then enable **Reconnect automatically** and connect. The scanner checks the verified vendor, device type, product code, and serial number before each Forward_Open. Automatic reconnect is off by default and applies only to input-only heartbeat profiles with zero output and configuration bytes and O→T Run/Idle disabled. After a receive watchdog timeout or transport I/O error, the scanner waits 1, 2, then 4 seconds for up to three retries per manual connection. An identity mismatch or CIP rejection ends the retry sequence. **Disconnect** or **Cancel reconnect** stops retries for that device.

Keep capture armed to keep the original capture start and sample indexes across a reconnect. The next successful connection uses a new CSV generation. Live packet and spacing counters describe the current connection. A disk error disarms only the failed GUI capture and shows its unconfirmed row count. The scanner can still show live inputs, and other devices continue independently.

### Connect to a bench adapter

1. Enter the adapter IPv4 address and the local IPv4 address assigned to the scanner NIC. Choose **Read identity** to verify the target before opening I/O.
2. For EDS setup, enter the vendor EDS path, choose **Import**, and select a connection profile. The importer prefers an available input-only profile. Expand **EDS source values and fields to review** for the imported values and fields needing input. The editable settings below override the imported values. The importer supports literal assembly paths and single-byte bracketed `ParamN` substitutions with declared defaults. It flags incompatible connection masks and unresolved paths.
3. For manual setup, choose **Manual settings** or load a TOML profile. Set the T→O input and O→T output or heartbeat connection points. Use payload sizes excluding transport headers, and confirm the sizes against the adapter. Enter each RPI in microseconds. Port zero requests an ephemeral local UDP port. Enter the selected NIC's fixed UDP port if the adapter requires it.
4. Set the Run/Idle option for each direction. Standard Forward_Open allows up to 509 bytes of assembly payload and optional Run/Idle header per direction after the two-byte Class 1 sequence. Enter the optional configuration instance and its hex bytes when required. A configuration payload is applied in Forward_Open. Review the exact bytes before reconnecting to apply changes.
5. Prefer a supported **Input-only heartbeat profile** for logging. An exclusive-owner connection can take control immediately and sends the staged bytes. An O→T Run/Idle header asserts Run from the first frame. Confirm ownership and the specific output or configuration bytes before checking the authorization box. On an authorized output connection, **Apply staged outputs** swaps the cyclic buffer.
6. Under **Assembly mappings**, define named inputs and outputs. The right-hand **Input assembly** panel shows the live named values and the raw bytes. Save the mapping and connection settings in a named TOML profile.

Open **Offline profile check** to review the effective assemblies, requested intervals, validation errors, and whether connecting would request output ownership, assert Run, or send configuration bytes. For a selected EDS profile, the check labels imported values, overrides, and manual entries. A size taken from an EDS assembly definition is a layout width. Verify the actual payload configured on the device before connecting.

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

The live panel separates requested RPIs, Forward_Open actual intervals and socket timing. Spacing percentiles use exact microsecond buckets through 32,767 µs and round longer values up by less than 0.4%. The longest gap is measured directly. Capture stats separate intentionally skipped unchanged packets, sequence gaps, duplicates, late and invalid packets, sender deadline skips, local buffer overruns and disk errors. A local overrun leaves a gap in the sample indexes. The queue reserves space up front for packet snapshots. Increasing its sample capacity increases memory use.

### CLI

The console executable uses the same EDS importer, scanner and CSV writer as the GUI. `eds` lists imported profiles, resolved settings, assembly definition widths, and unsupported features offline. `validate` checks a TOML profile offline and optionally compares it to one EDS connection profile. It reports connection validity and capture configuration separately. `identity` binds to the local IPv4 address and reads the adapter's CIP Identity Object. `capture` checks the expected decimal vendor, device type and product code in the same session before Forward_Open. Use a verified input-only TOML profile with zero output and configuration bytes. The new capture directory's parent must already exist.

```powershell
.\target\release\eipmaster-cli.exe eds samples\loopback.eds
.\target\release\eipmaster-cli.exe validate samples\loopback.toml samples\loopback.eds Connection1
.\target\release\eipmaster-cli.exe identity 127.0.0.1 127.0.0.1
.\target\release\eipmaster-cli.exe capture samples\loopback.toml .\.local\console-capture 10 0:12:1
```

Start `python tools/loopback_adapter.py .local/adapter.json` in another console before each localhost `identity` or `capture` example. Create `.local` first if needed. For a bench run, pass the verified adapter and NIC addresses to `identity` and use the corresponding TOML profile for `capture`. The CLI uses the profile's buffer capacity, rotation limits and scheduler when present, and its explicit output directory argument takes precedence. Append the decimal serial number as a fourth field in the expected identity when you need to pin a particular unit. Capture runs for the requested number of seconds after the first accepted packet. Results are printed as TOML and saved to `capture.toml` beside the CSV. The report includes the last accepted assembly payload in hex alongside its decoded value. It separates requested and actual intervals, socket spacing, sequence gaps, skipped send slots, unchanged samples and CSV overruns. A failed or incomplete capture returns a nonzero exit code. Review the loss counters after a completed capture.

For headless recovery, set `auto_reconnect = true` in the profile's `[app]` table. The CLI verifies the requested vendor, device type, and product code on the first session and pins its serial number for retries. The original capture deadline includes reconnect time. The report totals counters across attempts and includes an `[[attempts]]` entry with actual intervals and observed spacing for each generation. Top-level actual intervals and spacing refer to the final attempt. If the deadline expires during an outage, the CLI reports the incomplete capture and exits nonzero.

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

### Run the headless bench sequence

`qualify` uses an input-only profile and a verified decimal `vendor:type:product:serial`. It checks the identity in the same TCP session before every Forward_Open. It runs 15 seconds at 8 ms, followed by three 60-second captures at 1 ms. Each run sets both directional RPIs, records every valid packet, and uses a fresh connection. The source profile is preserved.

```powershell
$profile = '.\.local\bench-input.toml'
$expected = Read-Host 'Verified decimal vendor:type:product:serial'
.\target\release\eipmaster-cli.exe qualify $profile .\.local\bench-direct-01 direct $expected
```

Use `direct` or `switched` to record the path you connected and keep the results in separate new directories. Use `loopback` with two loopback addresses for a simulator. Only the loopback mode accepts an optional final duration for quick checks.

The new directory contains the effective profiles, each run's CSV and capture report, and `qualification.toml`. The summary records the active run and saves each result as it completes. A short or rejected baseline stops the sequence, as does a baseline RPI mismatch. Loss observed during a completed baseline is recorded and the 1 ms trials continue. An identity or disk error stops further connections. Exit code zero requires all four runs to finish cleanly, negotiate the requested intervals, and report zero packet anomalies or lost samples. Sender deadline skips and spacing are reported separately. These are headless results. [Physical GUI acceptance](VERIFICATION.md#physical-bench-acceptance) has its own test conditions.

## Verification

Run the Rust tests and the loopback harness on Windows. The [verification notes](VERIFICATION.md) describe the upstream crate review, simulator timing and physical acceptance procedure.
The CLI functional check defaults to an 8 ms RPI. Pass `--rpi-us 1000` to exercise the same cases at 1 ms. The GUI harness accepts `--dual-only` to run two independent loopback adapters and stall one while the other continues recording.

```powershell
cargo +stable-x86_64-pc-windows-msvc test --locked --release
python tools/test_cli.py .\target\release\eipmaster-cli.exe .\.local\cli-check
python tools/test_cli.py .\target\release\eipmaster-cli.exe .\.local\qualification-check --qualification
python tools/test_recovery.py .\target\release\eipmaster-cli.exe .\.local\recovery-check
```

The [independent OpENer compatibility check](VERIFICATION.md#opener-compatibility) builds a pinned software adapter and runs a headless 60-second input-only capture using the upstream EDS.

## Roadmap

The [roadmap](ROADMAP.md) tracks the single-device preview, multiple adapters, and device compatibility. GitHub milestones link the work and its acceptance criteria.

## Maintainers

[votecoffee](https://github.com/votecoffee)

## Contributing

Use the [issue tracker](https://github.com/votecoffee/EIPMaster/issues) for reproducible bugs and feature requests. Include the Windows build, the requested and actual intervals, and sanitized logs or a loopback reproduction when reporting a connection issue.

## License

[MIT © votecoffee](LICENSE)
