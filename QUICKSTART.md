# EIPMaster Windows x64 preview

Extract the entire ZIP into a new folder and open PowerShell there. Run the GUI from that folder so the sample paths resolve.

```powershell
.\assembly-scanner.exe
```

The GUI starts offline. It uses an OpenGL-capable graphics driver and remembers the last setup in `%LOCALAPPDATA%\EIPMaster\last-session.toml`. Python 3.11 or newer is needed only for the optional simulator and CSV utilities.

## Check the bundle on localhost

```powershell
.\eipmaster-cli.exe eds .\samples\loopback.eds
.\eipmaster-cli.exe validate .\samples\loopback.toml .\samples\loopback.eds Connection1
python tools/test_cli.py .\eipmaster-cli.exe .\loopback-check
```

The simulator check uses a new output directory and an 8 ms RPI. It exercises input-only capture, named fields, on-change logging and CSV rotation over localhost.

## Connect to an isolated bench adapter

1. Enter the adapter and local NIC IPv4 addresses and select **Read identity**. Verify the target before opening I/O.
2. Import the vendor EDS or load a saved TOML profile. Confirm the actual assembly sizes, connection points, Run/Idle settings and RPIs against the device. **Offline profile check** shows the effective settings and write implications.
3. Prefer a supported **Input-only heartbeat profile** for logging. Review the staged bytes and grant write authorization only when testing an approved output or configuration change.
4. Select the input mappings and CSV fields, then start capture and connect. Use **Stop capture and flush** before joining rotated parts.

```powershell
python tools/join_capture.py 'captures/capture-<UTC-prefix>' joined.csv
```

Use the capture prefix shown by the GUI. [Verification notes](VERIFICATION.md) describe the timing measurements and [physical acceptance](https://github.com/votecoffee/EIPMaster/issues/2).

## Verify the download

Compare `Get-FileHash -Algorithm SHA256` for the downloaded ZIP with its `.zip.sha256` file. The extracted `SHA256SUMS.txt` records hashes for the individual files. `BUILD-INFO.json` identifies the source commit, toolchain, and locked dependency manifest. License information is in [LICENSE](LICENSE) and the [third-party notice index](third-party/INDEX.md).
