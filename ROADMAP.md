# EIPMaster roadmap

EIPMaster is a standalone Windows x64 EtherNet/IP scanner. The current GUI can run multiple fixed-size cyclic Class 1 unicast connections with a separate setup and capture per device. It supports EDS and manual setup, saved profiles, typed assembly mappings, authorized output staging, a GUI and CLI, and selective CSV logging with rotation and loss accounting.

Simulator checks cover that workflow. Physical acceptance of sustained 1 ms capture in EIPMaster remains open. Treat the requested RPI, the intervals returned by Forward_Open, and the measured send and receive spacing as separate results.

## 0.2 Single-device preview

1. [Improve Windows 1 ms producer timing](https://github.com/votecoffee/EIPMaster/issues/1). Completed the wait-handle reuse implementation and paired [localhost timing comparison](VERIFICATION.md#windows-wait-handle-reuse) with the GUI and logger active.
2. [Add offline profile preflight](https://github.com/votecoffee/EIPMaster/issues/3). The GUI check and CLI `validate` report effective connection settings, output ownership, assembly bounds, and EDS-derived sizes before opening I/O.
3. [Qualify the physical bench path](https://github.com/votecoffee/EIPMaster/issues/2). When the bench adapter is available, verify identity and start at 8 ms in input-only mode. A GUI identity read pins the unit for the next manual connection. Attempt three 60-second captures at 1 ms on the direct link. Record the same timing and loss metrics and qualify any intended switched path separately. The [headless qualification runner](README.md#run-the-headless-bench-sequence) collects this evidence without opening a window. It labels its results separately from GUI acceptance. Both paths report observed packet rates, spans, and counts of intervals at or above two and four times the negotiated RPI alongside packet and sample losses. The GUI and CLI enforce timed CSV cutoffs using packet timestamps from the first input. The GUI can save a run report beside the CSV after disconnect and flush. Both reports record the configured CIP timeout code, the receive watchdog calculated from the negotiated input interval, and the separate Forward_Close outcome. Timeout diagnostics also show O-to-T sends and receive-check gaps during input silence. Startup timing shows the explicit exchange, the first O-to-T send, the first received UDP datagram, and the first valid input. Both reports include actual UDP endpoints and Class 1 connection IDs for correlation with a packet trace. Each report also summarizes the values and changes in the saved input columns. The CSV join tool can verify the saved parts against either report.
4. [Package a Windows x64 preview](https://github.com/votecoffee/EIPMaster/issues/4). Use the checked-in packager to build with locked dependencies. Test the GUI and CLI from a fresh unpacked folder. Include generic samples, licensing, build identity, and the current hardware verification status.

The offline timing and profile work can proceed while the bench device is unavailable. Physical qualification requires completed captures with the GUI and logger active, clean close, and no watchdog timeout, local buffer overrun, or unexplained packet loss under the recorded bench conditions. [Single-device milestone](https://github.com/votecoffee/EIPMaster/milestone/1)

## 0.3 Multiple adapters

- [Run two isolated scanner sessions](https://github.com/votecoffee/EIPMaster/issues/5). The GUI keeps each adapter's setup, sockets, identity, staged outputs, and write authorization separate. The [dual-adapter loopback check](VERIFICATION.md#concurrent-adapter-sessions) exercises a receive timeout while the other session continues.
- [Capture and recover streams independently](https://github.com/votecoffee/EIPMaster/issues/6). Each device has its own bounded CSV queue, rotation, loss counters, and reconnect generations. Optional reconnect verifies the adapter's identity on every attempt. The GUI run report retains the timing, errors, and final state of each capture generation. The [headless recovery check](VERIFICATION.md#headless-recovery) exercises reconnect and writer failure alongside a healthy stream.

The milestone requires concurrent loopback coverage and compatibility with existing single-device profiles. [Multiple-adapter milestone](https://github.com/votecoffee/EIPMaster/milestone/2)

## 0.4 Adapter compatibility

- [Verify a second documented adapter family](https://github.com/votecoffee/EIPMaster/issues/7). The [OpENer software adapter check](VERIFICATION.md#opener-compatibility) covers the upstream EDS, identity, assembly decoding, and sustained unicast input-only capture. Physical device acceptance remains separate.
- Prioritize Large Forward_Open, electronic keys, routed paths, or multicast when a documented device needs them. Pair each protocol extension with a fixture and device-level evidence.

Device profiles, vendor files, and raw captures belong in the ignored `.local/` directory. [Adapter-compatibility milestone](https://github.com/votecoffee/EIPMaster/milestone/3)
