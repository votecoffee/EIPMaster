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

### Select the cyclic producer scheduler

**Deadline** is the default. On Windows, **Deadline (above normal)** uses the same schedule and requests above-normal priority for that connection's cyclic producer thread. Select it in the device setup or use `schedule = "deadline_above_normal"` in the profile's `[app]` table. The app reports whether the request applied. **Tokio baseline** uses Tokio's skipped-tick scheduler.

The dashboard and CLI `send_timing` table separate the worst dispatch lateness from the longest wall-clock UDP send call. They also count send calls lasting at least the negotiated O→T interval. Dispatch lateness is available for deadline schedules. Missed send slots and packet sequence gaps are counted separately. [Headless priority comparison](VERIFICATION.md#producer-timing-and-thread-priority)

### Recover an input-only session

Select **Read identity**, verify the adapter, then enable **Reconnect automatically** and connect. The scanner checks the verified vendor, device type, product code, and serial number before each Forward_Open. Automatic reconnect is off by default and applies only to input-only heartbeat profiles with zero output and configuration bytes and O→T Run/Idle disabled. After a receive watchdog timeout or transport I/O error, the scanner waits 1, 2, then 4 seconds for up to three retries per manual connection. An identity mismatch or CIP rejection ends the retry sequence. **Disconnect** or **Cancel reconnect** stops retries for that device.

Keep capture armed to keep the original capture start and sample indexes across a reconnect. The next successful connection uses a new CSV generation. Live packet and spacing counters describe the current connection. A disk error disarms only the failed GUI capture and shows its unconfirmed row count. The scanner can still show live inputs, and other devices continue independently.

### Connect to a bench adapter

1. Enter the adapter IPv4 address and the local IPv4 address assigned to the scanner NIC.
2. For EDS setup, enter the vendor EDS path, choose **Import**, and select a connection profile. The importer prefers an available input-only profile. Expand **EDS source values and fields to review** for the imported values and fields needing input. The editable settings below override the imported values. The importer supports literal assembly paths and single-byte bracketed `ParamN` substitutions with declared defaults. It flags incompatible connection masks and unresolved paths.
3. For manual setup, choose **Manual settings** or load a TOML profile. Set the T→O input and O→T output or heartbeat connection points. Use payload sizes excluding transport headers, and confirm the sizes against the adapter. Enter each RPI in microseconds. Port zero requests an ephemeral local UDP port. Enter the selected NIC's fixed UDP port if the adapter requires it.
4. Set the Run/Idle option for each direction. Standard Forward_Open allows up to 509 bytes of assembly payload and optional Run/Idle header per direction after the two-byte Class 1 sequence. Enter the optional configuration instance and its hex bytes when required. A configuration payload is applied in Forward_Open. Review the exact bytes before reconnecting to apply changes.
5. After setting up the profile, choose **Read identity** while disconnected and verify the returned device.
6. Prefer a supported **Input-only heartbeat profile** for logging. An exclusive-owner connection can take control immediately and sends the staged bytes. An O→T Run/Idle header asserts Run from the first frame. Confirm ownership and the specific output or configuration bytes before checking the authorization box. On an authorized output connection, **Apply staged outputs** swaps the cyclic buffer.
7. Under **Assembly mappings**, define named inputs and outputs. The right-hand **Input assembly** panel shows the live named values and the raw bytes. Save the mapping and connection settings in a named TOML profile.

A successful identity read pins the vendor, device type, product code, and serial number for the next manual connection. **Connect** checks the pin in the TCP session that will carry Forward_Open and stops before opening I/O if the identity differs. A fresh successful read updates the pin. A failed refresh retains the prior pin. Editing the adapter or NIC address or loading a profile clears it.

Open **Offline profile check** to review the effective assemblies, requested intervals, validation errors, and whether connecting would request output ownership, assert Run, or send configuration bytes. For a selected EDS profile, the check labels imported values, overrides, and manual entries. A size taken from an EDS assembly definition is a layout width. Verify the actual payload configured on the device before connecting.

The initial protocol scope is standard Forward_Open, fixed-size cyclic Class 1 unicast and direct assembly paths. Routed paths, electronic keys, multicast, listen-only, and Large Forward_Open are outside that scope. Keep device-specific presets, vendor files and raw captures in the Git-ignored `.local/` folder. The tracked [sample EDS](samples/loopback.eds) and [sample TOML](samples/loopback.toml) target the localhost fixture.

Set **CIP timeout code** in the GUI or `timeout_multiplier_code` in the TOML profile. Codes 0 through 7 represent factors of 4 through 512 in powers of two. Profiles that omit the setting use code 3 (32 times the interval). The app sends the code in Forward_Open and uses the returned input interval times that factor for its local receive watchdog. Before the first valid input, the scanner waits for the larger of two seconds and two returned input intervals. Preflight shows the watchdog calculated from the requested interval. Capture reports also show the local watchdog calculated from the interval the adapter returned.

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

Startup timing separates the Forward_Open exchange from the first successful O-to-T socket send, the first datagram received on the I/O socket, and the first valid T-to-O input. The three I/O timings use the end of the exchange as their reference. The first datagram may be malformed or from another sender. The GUI displays these timings, and GUI and CLI capture reports save them in microseconds under `startup_timing` for each reported attempt. A rejected Forward_Open records the exchange time. Missing events are omitted from the report. The measurements use the scanner host's monotonic clock at socket calls and do not measure wire time.

The live panel separates requested RPIs, Forward_Open actual intervals and socket timing. Spacing percentiles use exact microsecond buckets through 32,767 µs and round longer values up by less than 0.4%. The longest gap is measured directly. Within each connection attempt, each direction reports the elapsed span from its first to its last packet and counts observed intervals at or above two and four times its Forward_Open interval. The four-times count is also included in the two-times count. These counters describe socket timing and are separate from sequence gaps and CSV loss. Terminal watchdog silence is reported separately. The observed packet rate divides the interval count by the first-to-last packet span. Each direction reports packets per second and its percentage of the corresponding rate from Forward_Open.

Capture stats separate intentionally skipped unchanged packets, sequence gaps, duplicates, late and invalid packets, sender deadline skips, local buffer overruns and disk errors. The device panel shows the terminal silence and threshold when valid input times out. A local overrun leaves a gap in the sample indexes. The queue reserves space up front for packet snapshots. Increasing its sample capacity increases memory use.

Set **Stop after seconds (0 = manual)** under **CSV capture** to arm a timed recording of up to 3,600 seconds. The timer starts with the first valid input after arming and includes time spent reconnecting. The recorder accepts packets before the deadline, then the GUI flushes the queue. **Disconnect** is a separate control. The GUI saves the setting as `[app].gui_capture_seconds`. The CLI uses the duration in its command.

For a timed GUI bench run, start CSV capture before connecting. When the timer stops the capture, disconnect and wait for the final connection state, then select **Save run report**. For a manual stop, select **Stop capture and flush** before saving. Timed reports record the requested duration and whether it was reached. They also count post-deadline packets that reached the recorder before it detached. The app creates a `-report.toml` file beside the CSV parts. It records the adapter identity, requested and actual intervals, spacing, transport errors, Forward_Close outcome, and CSV counts. Saving a report preserves any file already at that path.

After the CSV writer flushes successfully, the GUI and CLI reports include `field_summaries` for each logged column. Each summary preserves the assembly selection, first and last saved values, and finite numeric minimum and maximum across all CSV parts. It counts changes between adjacent saved rows within each connection generation. Values are stored as text to preserve the CSV cell values and the full range of unsigned integers.

The GUI report's `[[attempts]]` entries cover the full lifetime of each connection that overlapped the capture, in generation order. Match the attempt's `generation` to `connection_generation` in the CSV. The `[connection]` section repeats the latest attempt. The `[capture]` section totals the CSV counts across all parts and records the range of connection generations while CSV was armed.

### CLI

The console executable uses the same EDS importer, scanner and CSV writer as the GUI. `eds` lists imported profiles, resolved settings, assembly definition widths, and unsupported features offline. `validate` checks a TOML profile offline and optionally compares it to one EDS connection profile. It reports connection validity and capture configuration separately. `identity` binds to the local IPv4 address and reads the adapter's CIP Identity Object. `capture` checks the expected decimal vendor, device type and product code in the same session before Forward_Open. Use a verified input-only TOML profile with zero output and configuration bytes. The new capture directory's parent must already exist.

```powershell
.\target\release\eipmaster-cli.exe eds samples\loopback.eds
.\target\release\eipmaster-cli.exe validate samples\loopback.toml samples\loopback.eds Connection1
.\target\release\eipmaster-cli.exe identity 127.0.0.1 127.0.0.1
.\target\release\eipmaster-cli.exe capture samples\loopback.toml .\.local\console-capture 10 0:12:1
```

Start `python tools/loopback_adapter.py .local/adapter.json` in another console before each localhost `identity` or `capture` example. Create `.local` first if needed. For a bench run, pass the verified adapter and NIC addresses to `identity` and use the corresponding TOML profile for `capture`. The CLI uses the profile's buffer capacity, rotation limits and scheduler when present, and its explicit output directory argument takes precedence. Append the decimal serial number as a fourth field in the expected identity when you need to pin a particular unit. CSV records the requested interval starting with the first accepted packet. Packets at or after the cutoff are counted as `after_capture_deadline` while the connection stops. Results are printed as TOML and saved to `capture.toml` beside the CSV. The `accepted` count, spacing, and last input payload describe the connection. The `seen`, `written`, and field summaries describe the recording window. It separates requested and actual intervals, socket spacing, sequence gaps, skipped send slots, unchanged samples and CSV overruns. A successful capture requires a matching Forward_Close reply on its final connection. The `forward_close` table separates confirmed, rejected, and unconfirmed closes from receive timeouts and earlier connection errors. A failed or incomplete capture returns a nonzero exit code. On Windows, press **Ctrl+C** or **Ctrl+Break** in the running console to stop. The CLI requests Forward_Close, flushes pending CSV rows, writes `interrupted = true` in the capture report, and exits with code 130. Review the loss counters after a completed capture. A `receive_timeout` table records whether the scanner was still waiting for its first valid packet, how long valid input was absent, the watchdog threshold, and the most recent datagram age and valid sequence when available. The terminal silence is separate from packet-spacing percentiles.

For headless recovery, set `auto_reconnect = true` in the profile's `[app]` table. The CLI verifies the requested vendor, device type, and product code on the first session and pins its serial number for retries. The original capture deadline includes reconnect time. The report totals counters across attempts and includes an `[[attempts]]` entry with actual intervals and observed spacing for each generation. Top-level actual intervals, spacing, and send timing refer to the final attempt. If the deadline expires during an outage, the CLI reports the incomplete capture and exits nonzero.

Use the capture prefix shown in the GUI to join the parts in order after stopping capture.

```powershell
python tools/join_capture.py 'captures/capture-<UTC-prefix>' joined.csv
```

Use `--report` with a stopped capture's `capture.toml` or GUI `-report.toml`. For qualification, use the individual trial's capture report. Omit the output path to verify the parts without writing a joined file.

```powershell
python tools/join_capture.py 'captures/capture-<UTC-prefix>' --report captures/capture.toml
python tools/join_capture.py 'captures/capture-<UTC-prefix>' joined.csv --report captures/capture.toml
```

Report checking compares the prefix name, written row count, and the saved start time, part count, and first and last field values when available. It counts logged connection generations and reconciles sample-index gaps with recorded buffer overruns. If verification fails, the command discards any partial join it created.

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

The new directory contains the effective profiles, each run's CSV and capture report, and `qualification.toml`. The summary records the active run and saves each result as it completes. A short or rejected baseline stops the sequence, as does a baseline RPI mismatch. Loss observed during a completed baseline is recorded and the 1 ms trials continue. An identity or disk error, a rejected Forward_Close, or an unconfirmed close stops further trials. Exit code zero requires all four runs to finish cleanly, negotiate the requested intervals, and report zero packet anomalies or lost samples. Sender deadline skips and spacing are reported separately. A console interrupt stops the active run, prevents further trials, and saves an interrupted qualification receipt. These are headless results. [Physical GUI acceptance](VERIFICATION.md#physical-bench-acceptance) has its own test conditions.

## Verification

Run the Rust tests and the loopback harness on Windows. The [verification notes](VERIFICATION.md) describe the upstream crate review, simulator timing and physical acceptance procedure.
The CLI functional check defaults to an 8 ms RPI. Pass `--rpi-us 1000` to exercise the same cases at 1 ms. The GUI harness accepts `--dual-only` to run two independent loopback adapters and stall one while the other continues recording.

```powershell
cargo +stable-x86_64-pc-windows-msvc test --locked --release
python tools/test_cli.py .\target\release\eipmaster-cli.exe .\.local\cli-check
python tools/test_cli.py .\target\release\eipmaster-cli.exe .\.local\qualification-check --qualification
python tools/test_recovery.py .\target\release\eipmaster-cli.exe .\.local\recovery-check
```

On Windows 11 build 26100 or later, run `python tools/test_interrupt.py .\target\release\eipmaster-cli.exe .\.local\interrupt-check` to exercise real console signals without opening a window.

The [independent OpENer compatibility check](VERIFICATION.md#opener-compatibility) builds a pinned software adapter and runs a headless 60-second input-only capture using the upstream EDS.

## Roadmap

The [roadmap](ROADMAP.md) tracks the single-device preview, multiple adapters, and device compatibility. GitHub milestones link the work and its acceptance criteria.

## Maintainers

[votecoffee](https://github.com/votecoffee)

## Contributing

Use the [issue tracker](https://github.com/votecoffee/EIPMaster/issues) for reproducible bugs and feature requests. Include the Windows build, the requested and actual intervals, and sanitized logs or a loopback reproduction when reporting a connection issue.

## License

[MIT © votecoffee](LICENSE)
