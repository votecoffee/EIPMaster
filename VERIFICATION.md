# Assembly Scanner 0.1.1 verification

## Windows build and upstream review

The package was built and run on Windows 11 Pro x64 build 26200 with an Intel Core Ultra 7 155H, using Rust 1.99.1 and the x64 MSVC toolchain. The PE header identifies a Windows GUI x64 executable. The import table uses Windows system DLLs and OpenGL. The application test suite passed five tests. The published ethernetip-connections 0.1.1 crate passed its 27 unit tests on the same Windows host.

- The published scanner uses Tokio TCP and UDP. It registers a session, opens a cyclic Class 1 point-to-point connection, uses the returned O→T actual interval for production, and exchanges CPF sequenced address and connected data items. Its producer uses a Tokio interval with missed ticks set to Skip. Its receiver updates a shared assembly snapshot and has no public per-packet timestamp/sequence callback. The scanner source also uses one Run/Idle flag in setup and decodes incoming data without an input Run/Idle header. [Published scanner source](https://docs.rs/crate/ethernetip-connections/0.1.1/source/src/scanner.rs)
- The application uses the published Forward_Open and Forward_Close codecs and the core TCP encapsulation and CPF codecs. Its connection worker binds both transports to the selected local IPv4 address. The independent OpENer source identifies 0x8000 as O→T Sockaddr Info and 0x8001 as T→O. Its fixed Class 1 connection size includes the two-byte sequence count. These wire details differ from the crate's stock scanner. [OpENer CPF constants](https://github.com/EIPStackGroup/OpENer/blob/0e1ab7ee5c272950c0dc8daf7793008d7a518912/source/src/enet_encap/cpf.h), [OpENer I/O connection source](https://github.com/EIPStackGroup/OpENer/blob/0e1ab7ee5c272950c0dc8daf7793008d7a518912/source/src/cip/cipioconnection.c), [ODVA CPF wire overview](https://www.odva.org/wp-content/uploads/2020/06/PUB00123R1_Common-Industrial_Protocol_and_Family_of_CIP_Networks.pdf)
- The prototype has separate producer, receive and CSV writer threads. The deadline producer uses absolute monotonic deadlines, a sleep with a short spin guard, and skips missed slots. The Tokio baseline uses the stock interval policy on the same application packet path and socket. This comparison isolates the scheduler policy. The stock scanner's full stack was evaluated from source and unit tests.

## Simulator timing

The independent Python adapter and the real Windows GUI ran on the same host over localhost, with CSV capture and three-second rotation active. Each run was about 12 seconds. Both directions requested 1,000 µs in both runs and Forward_Open returned 1,000 µs for each direction. All spacing below is in microseconds.

| Scheduler | Measurement point | p50 | p95 | p99 | Longest |
|---|---|---:|---:|---:|---:|
| Deadline | Scanner O→T send | 1,022 | 1,153 | 1,424 | 2,547 |
| Deadline | Fixture O→T receive | 1,019 | 2,020 | 2,170 | 3,498 |
| Deadline | Scanner T→O receive | 1,006 | 1,176 | 1,372 | 2,473 |
| Tokio Skip | Scanner O→T send | 1,029 | 1,639 | 2,123 | 3,142 |
| Tokio Skip | Fixture O→T receive | 1,023 | 2,115 | 2,544 | 3,434 |
| Tokio Skip | Scanner T→O receive | 1,004 | 1,225 | 1,466 | 2,786 |

| Scheduler | Scanner O→T sent / fixture received | Fixture T→O sent / scanner accepted / CSV written | Sequence gaps O→T / T→O | CSV overruns |
|---|---:|---:|---:|---:|
| Deadline | 11,893 / 11,893 | 11,897 / 11,897 / 11,897 | 0 / 0 | 0 |
| Tokio Skip | 11,889 / 11,889 | 11,880 / 11,880 / 11,880 | 0 / 0 | 0 |

Both runs had zero duplicates, late packets, invalid frames, timeouts and disk errors. The scanner deadline loop skipped 5 scheduled send slots. The Tokio baseline reports no missed-slot count. The fixture producer skipped 2 slots during the deadline run and 9 during the Tokio run. Skipped scheduler slots occur before a packet sequence is assigned and are reported independently from sequence gaps. Timing varied across runs and process load. In these runs, the deadline policy lowered scanner send p99 by approximately 33%.

## Windows wait-handle reuse

The deadline producer on Windows now reuses one high-resolution waitable timer per producer. It keeps the same monotonic deadline, final spin guard, and missed-slot policy. If the dedicated timer is unavailable, the producer uses the standard sleep path. The Tokio baseline is unchanged. Rust's Windows sleep already attempts a high-resolution timer and creates one per call. [Rust Windows sleep source](https://github.com/rust-lang/rust/blob/master/library/std/src/sys/thread/windows.rs), [Windows high-resolution timer API](https://learn.microsoft.com/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw)

The previous build (`55fa1c0`) and reused-handle build ran three trials each, in alternating pair order, on the same Windows host. The independent localhost fixture, GUI, and rotating CSV writer were active for about 12 seconds per trial. Both directions requested and negotiated 1,000 µs in all six trials. Each cell shows the range of the three per-run measurements in microseconds.

| Build | Measurement point | p50 | p95 | p99 | Longest |
|---|---|---:|---:|---:|---:|
| Previous | Scanner O→T send | 1,025–1,029 | 1,191–1,231 | 1,481–1,538 | 2,548–7,528 |
| Reused handle | Scanner O→T send | 1,021–1,039 | 1,168–1,234 | 1,458–1,473 | 2,573–14,472 |
| Previous | Fixture O→T receive | 1,023–1,028 | 2,037–2,084 | 2,220–2,312 | 3,164–7,627 |
| Reused handle | Fixture O→T receive | 1,020–1,034 | 2,037–2,088 | 2,236–2,293 | 3,544–15,126 |
| Previous | Fixture T→O send | 1,008–1,014 | 1,202–1,231 | 1,418–1,515 | 2,475–6,933 |
| Reused handle | Fixture T→O send | 1,008–1,017 | 1,174–1,217 | 1,418–1,452 | 2,535–15,281 |
| Previous | Scanner T→O receive | 1,008–1,014 | 1,202–1,240 | 1,419–1,520 | 2,481–6,900 |
| Reused handle | Scanner T→O receive | 1,008–1,016 | 1,181–1,217 | 1,422–1,451 | 2,538–15,194 |

| Build | Scanner skipped send slots | Fixture skipped send slots | Sequence gaps O→T / T→O | CSV rows | CSV overruns | Timeouts |
|---|---:|---:|---:|---:|---:|---:|
| Previous | 86 | 123 | 0 / 2 | 35,532 | 0 | 0 |
| Reused handle | 46 | 36 | 0 / 0 | 35,538 | 0 | 0 |

All runs closed cleanly. The median per-run scanner send p99 changed from 1,535 to 1,464 µs. The candidate's longest send gap was worse in one trial, when the fixture also recorded a long send gap. This comparison qualifies the offline implementation only. [Physical bench acceptance](https://github.com/votecoffee/EIPMaster/issues/2) remains open.

## Offline functional checks

- The Rust tests exercised signed, floating-point and bit decoding, endian order and out-of-range selection, plus EDS profile parsing, unresolved paths, and safe single-byte ParamN path resolution.
- An eight-sample ring and an intentionally delayed writer accepted eight of 100 offered samples, counted 92 overruns and flushed the eight accepted rows across size-rotated parts on stop. A forced collision at the second time-rotated part left the original file intact, raised a disk error and reported one flushed row out of two seen.
- The GUI fixture checked rotation, strict CSV parsing, repeated values and sample-index continuity. Packet fault injection exercised a skipped sequence, duplicate, delayed sequence and invalid payload. The stall case counted a receive timeout and performed Forward_Close. Forward_Open rejection preserved CIP general status 0x01 and extended status 0x0109 in the connection error field.
- With requests of 1,000 / 1,000 µs, the fixture also returned different intervals of 1,250 / 1,500 µs for O→T / T→O. The app reported the returned values separately and used them for its cyclic timing.
- A loopback-only exclusive-owner test checked the padded configuration segment, Run/Idle framing in each direction, and the transition between two staged output buffers. The reconnect test kept one CSV capture across two connections and verified the generation change with contiguous sample indexes.

Historical simulator reports were bundled with the original development packages. The portable preview includes this summary and its build manifest. Device-specific captures stay outside the package.

## Concurrent adapter sessions

The Windows GUI ran two independent localhost fixtures in a single process. The adapters used distinct serial numbers, UDP endpoints, and input values. The healthy adapter requested 8,000 µs in both directions and the stalled adapter requested 16,000 µs. Each fixture returned the same intervals in Forward_Open.

| Stream | End state | CSV rows / parts | T→O sequence gaps | CSV overruns |
|---|---|---:|---:|---:|
| Healthy | Closed | 1,476 / 4 | 1 | 0 |
| Stalled | Timed out | 75 / 1 | 0 | 0 |

A repeat wrote 1,483 healthy and 75 stalled rows with zero sequence gaps or overruns on either stream.

Both fixtures received Forward_Close. Each stream had contiguous CSV sample indexes and every accepted sample was written to the correct stream. The captures overlapped and the healthy stream recorded for more than five seconds after the stalled stream's last sample. The receive timeout was counted only on the stalled adapter. These results cover simulated input-only sessions.

```powershell
python tools/benchmark.py .\target\release\assembly-scanner.exe .\.local\dual-adapter-check --dual-only
```

## Headless recovery

Three concurrent Windows CLI captures used separate localhost fixtures at an 8 ms RPI in both directions. Forward_Open returned 8 ms for both directions. One fixture stalled and resumed after its first reconnect delay. A second fault was injected by placing an existing file at the disk stream's next CSV part.

| Stream | Result | CSV rows | Timeouts | T→O sequence gaps | CSV overruns |
|---|---|---:|---:|---:|---:|
| Healthy | Completed throughout both faults | 752 | 0 | 0 | 0 |
| Recovered | Completed across generations 1 and 2 | 468 | 1 | 0 | 0 |
| Disk fault | Reported the collision and exited incomplete | 250 | 0 | 0 | 0 |

A repeat recorded one sequence gap on the healthy stream and zero on the recovered stream. Both completed with zero overruns.

The recovered stream retained its UTC start and contiguous sample indexes. The disk stream saw 252 rows and confirmed 250 written. The existing file was preserved. A separate fixture changed its serial number after a timeout. The scanner preserved the 75 previously captured rows and rejected the new identity before Forward_Open. Rust tests cover the bounded retry delays, GUI retry cancellation, and releasing a failed GUI capture slot while another writer continues.

```powershell
python tools/test_recovery.py .\target\release\eipmaster-cli.exe .\.local\recovery-check
```

## OpENer compatibility

The Windows CLI captured from the upstream OpENer sample adapter, built with MSVC from revision `0e1ab7ee`. The test launcher binds all adapter sockets to 127.0.0.2, supplies a synthetic MAC address, and fills the 32-byte input assembly with values 0 through 31. The adapter uses the upstream Class 1 stack and sample connection points. [Upstream sample EDS](https://github.com/EIPStackGroup/OpENer/blob/0e1ab7ee5c272950c0dc8daf7793008d7a518912/data/opener_sample_app.eds) · [Upstream sample application](https://github.com/EIPStackGroup/OpENer/blob/0e1ab7ee5c272950c0dc8daf7793008d7a518912/source/src/ports/WIN32/sample_application/sampleapplication.c)

EDS Connection2 resolves to input assembly 100, heartbeat point 152, and configuration instance 151 with zero configuration bytes. Both directions requested 30,000 µs and Forward_Open returned 30,000 µs in both directions. The Identity Object reported vendor 1, type 12, product 65001, revision 2.3, and serial 123456789. The target trace recorded Forward_Close.

The first 60-second run wrote all 2,000 accepted samples with one receive sequence gap, zero duplicates, zero invalid packets, zero timeouts, and zero CSV overruns. Socket spacing was measured separately.

| Direction | p50 µs | p95 µs | p99 µs | Longest µs |
|---|---:|---:|---:|---:|
| Scanner O→T send | 29,988 | 30,486 | 35,967 | 46,692 |
| Scanner T→O receive | 30,412 | 31,886 | 32,435 | 60,705 |

The reproducible run with rotation wrote all 1,989 accepted samples across six CSV parts. It reported 12 receive sequence gaps and zero CSV overruns, duplicates, invalid packets or timeouts. OpENer logged 2,001 sends with no socket send error. This comparison does not identify where the missing datagrams were dropped. The receive p50, p95, p99 and longest gap were 30,726, 31,980, 32,895 and 62,853 µs, respectively.

This check exposed the previous 20,000 µs percentile ceiling. The fixed histogram retains exact microsecond buckets through 32,767 µs and reports a bounded upper estimate for longer intervals. CSV elapsed times independently reproduced the receive percentiles within that resolution.

To repeat the headless test, install CMake and the Visual Studio 2022 C++ toolchain. Use a fresh output directory and a full upstream Git checkout. The tool builds a local copy of the pinned revision and keeps the upstream EDS, logs and CSV parts in the test directory.

```powershell
git clone https://github.com/EIPStackGroup/OpENer.git .local/opener-upstream
python tools/test_opener.py .\target\release\eipmaster-cli.exe .\.local\opener-upstream .\.local\opener-check
```

## Headless bench sequence

The CLI `qualify` command preserves each trial's capture report and CSV alongside the effective profiles and a cumulative receipt. The short loopback check exercised the 8 ms baseline and all three 1 ms trials. A fixture that returned 1,250 and 1,500 µs on one target trial produced a failed qualification receipt and still ran the remaining trials. An interval mismatch on the baseline stopped before the target trials. A same-model serial mismatch stopped before Forward_Open, including when the serial changed after the baseline. A profile with configuration bytes was rejected before any connection attempt.

```powershell
python tools/test_cli.py .\target\release\eipmaster-cli.exe .\.local\qualification-check --qualification
```

A full headless loopback sequence requested 8,000 µs in both directions for the 15-second baseline and 1,000 µs for three 60-second trials. Forward_Open returned the corresponding requested intervals on all four runs. The receipt correctly reported a failed qualification.

| Run | Sample span | End state | CSV rows | Receive sequence gaps | Scanner missed send slots | Fixture missed send slots |
|---|---:|---|---:|---:|---:|---:|
| 8 ms baseline | 15.008 s | Closed | 1,849 | 28 | 0 | 0 |
| 1 ms trial 1 | 60.012 s | Closed | 50,758 | 93 | 5,178 | 9,162 |
| 1 ms trial 2 | 38.554 s | Timed out | 36,821 | 18 | 1,000 | 1,748 |
| 1 ms trial 3 | 16.599 s | Timed out | 15,144 | 24 | 849 | 1,470 |

| Run | Receive p50 / p95 / p99 / longest (µs) | Send p50 / p95 / p99 / longest (µs) |
|---|---:|---:|
| 8 ms baseline | 8,010 / 9,410 / 16,069 / 19,979 | 8,010 / 9,187 / 10,098 / 16,670 |
| 1 ms trial 1 | 1,044 / 2,485 / 4,698 / 26,259 | 1,054 / 2,001 / 2,764 / 7,728 |
| 1 ms trial 2 | 1,022 / 1,411 / 2,600 / 21,042 | 1,030 / 1,457 / 2,123 / 7,658 |
| 1 ms trial 3 | 1,031 / 1,768 / 3,372 / 22,904 | 1,034 / 1,755 / 2,473 / 6,515 |

The fixture received Forward_Close on all four runs. All accepted packets were present in the rotated CSV parts with contiguous sample indexes. There were zero buffer overruns and disk errors. Spacing describes intervals between received packets. The terminal watchdog silence has no later packet to measure as an interval. These results are from localhost with the GUI closed.

## Windows receive readiness and watchdog evidence

The previous full headless sequence ended twice at a 32 ms receive watchdog limit. In one run the fixture recorded a 39.777 ms send gap. In the other its longest measured send gap was 27.872 ms. The earlier reports did not correlate the exact watchdog event with a fixture send timestamp, so the immediate causes remain unconfirmed.

On Windows the receiver now uses Winsock `select` to wait for readable UDP data before calling `recvfrom`. The receiver requests a 2 ms idle wait and keeps the same watchdog limits. [Winsock readiness semantics](https://learn.microsoft.com/windows/win32/api/winsock2/nf-winsock2-select)

Three alternating 10-second pairs per RPI compared the timed receive build (`83f19bf`) with the readiness build on the same localhost fixture. The requested intervals and Forward_Open intervals matched the listed RPI in both directions.

| RPI | Receive path | Accepted and written | Sequence gaps | Per-run receive p99 range (µs) | Longest receive gap (µs) |
|---|---|---:|---:|---:|---:|
| 8 ms | Timed receive | 3,750 | 4 | 8,402–8,620 | 18,507 |
| 8 ms | Readiness | 3,753 | 0 | 8,494–8,623 | 9,834 |
| 1 ms | Timed receive | 29,894 | 6 | 1,492–1,622 | 11,812 |
| 1 ms | Readiness | 29,815 | 0 | 1,495–1,803 | 4,409 |

All runs closed cleanly with zero receive watchdog timeouts and zero CSV overruns. The fixture skipped 132 send slots in the timed 1 ms trials and 227 in the readiness trials. Skipped fixture slots occur before sequence assignment. The trials show fewer received sequence gaps with readiness on this host. They did not reproduce the earlier watchdog failures.

A separate 60-second readiness run requested 1,000 µs in both directions and received 1,000 µs in both directions from Forward_Open. It closed cleanly with 58,772 accepted and written rows, zero receive sequence gaps, zero watchdog timeouts, and zero CSV overruns.

| Measurement point | p50 µs | p95 µs | p99 µs | Longest µs |
|---|---:|---:|---:|---:|
| Scanner T→O receive | 1,047 | 1,365 | 1,818 | 4,923 |
| Scanner O→T send | 1,043 | 1,280 | 1,708 | 4,785 |
| Fixture T→O send | 1,046 | 1,369 | 1,805 | 5,573 |

The scanner skipped 524 send slots and the fixture skipped 1,243. The scan and CSV capture ran headlessly over localhost.

Timeout reports now include the threshold, terminal silence, time since any received UDP datagram, and the last valid sequence when available. The headless CLI tests distinguish a quiet first-input wait from a first-input wait with malformed traffic. The recovery test checks a cyclic watchdog's terminal sequence against its last CSV sample.

## Producer timing and thread priority

The sender records lateness when it reaches a deadline and wall time around the UDP send call. A long call can include time when Windows descheduled the thread. These measurements describe separate points and can come from different packets.

The opt-in deadline mode requests `THREAD_PRIORITY_ABOVE_NORMAL` only for the cyclic producer thread. The sender reports whether Windows accepted the request. [Windows thread scheduling priorities](https://learn.microsoft.com/windows/win32/procthread/scheduling-priorities)

Three alternating pairs ran two concurrent 12-second CLI captures with three-second CSV rotation. Both streams requested and negotiated 1,000 µs in both directions. The primary stream alternated between normal and above-normal deadline scheduling. The peer always used the normal deadline schedule. All twelve captures closed cleanly with zero sequence gaps, watchdog timeouts, or CSV overruns.

| Primary producer priority | Primary missed slots per run | Peer missed slots per run | Primary send p99 range (µs) | Primary maximum send gap (µs) |
|---|---:|---:|---:|---:|
| Normal | 5 / 77 / 27 | 7 / 60 / 18 | 1,230–1,412 | 4,325 |
| Above normal | 2 / 2 / 4 | 61 / 6 / 8 | 1,204–1,335 | 2,859 |

The primary's maximum dispatch lateness was 3,487 µs at normal priority and 1,736 µs at above-normal priority. Its longest UDP call measured 2,695 µs and 1,761 µs, respectively. Peer performance varied between pairs, and the first above-normal trial had 61 peer misses versus seven in its normal-priority pair. Normal priority remains the default.

A separate 60-second run used above-normal priority on the primary and normal priority on the peer. Both requested 1,000 µs and both negotiated 1,000 µs in each direction. Each wrote 59,627 accepted packets to seven CSV parts with zero sequence gaps, watchdog timeouts, or CSV overruns.

| Stream | Missed producer slots | O→T send p50 / p95 / p99 / longest (µs) | T→O receive p50 / p95 / p99 / longest (µs) |
|---|---:|---:|---:|
| Above-normal primary | 40 | 1,075 / 1,151 / 1,221 / 3,963 | 1,065 / 1,197 / 1,466 / 4,986 |
| Normal peer | 320 | 1,073 / 1,186 / 1,479 / 5,174 | 1,063 / 1,217 / 1,638 / 4,710 |

The fixture producers missed 384 and 383 slots, respectively. The primary's worst dispatch lateness was 2,817 µs and its longest UDP send call was 579 µs. The peer recorded 4,612 µs and 2,712 µs, with six send calls lasting at least one RPI. These are headless localhost measurements.

## Graceful console stops

The CLI integration test sent real Windows console events inside an isolated windowless console. The adapter fixture confirmed Forward_Close in every case.

| Event and state | Preserved CSV rows | Result |
|---|---:|---|
| Ctrl+Break after capture rotated | 131 across two parts | Exit 130 and interrupted capture report |
| Ctrl+C during the qualification baseline | 5 | Exit 130 and interrupted qualification receipt with no later trials |
| Ctrl+C during the automatic reconnect delay | 75 | Exit 130 and no new connection attempt |

All accepted rows reached disk with contiguous sample indexes and zero buffer overruns. The windowless test harness requires Windows 11 build 26100 or later. It verifies the process list before sending either event. [Windows console control events](https://learn.microsoft.com/windows/console/ctrl-c-and-ctrl-break-signals)

```powershell
python tools/test_interrupt.py .\target\release\eipmaster-cli.exe .\.local\interrupt-check
```

## Packet span and long intervals

A headless 60-second Windows loopback capture requested 1,000 microseconds in both directions. Forward_Open returned 1,000 microseconds in both directions. The capture closed cleanly and wrote all 58,770 accepted rows across seven CSV parts with contiguous sample indexes, zero receive sequence gaps, zero watchdog timeouts, and zero CSV overruns.

| Socket measurement | Observed span (s) | p50 / p95 / p99 / longest (microseconds) | Intervals at or above 2x / 4x actual RPI |
|---|---:|---:|---:|
| O-to-T send | 60.013080 | 1,022 / 1,326 / 1,825 / 15,697 | 492 / 86 |
| T-to-O receive | 60.013486 | 1,007 / 1,288 / 1,810 / 20,479 | 481 / 92 |

The scanner skipped 1,128 scheduled sends and the fixture skipped 1,244. Both skip slots before assigning a sequence. The long-interval counts describe socket timing even when the sequence is contiguous. The 4x counts are included in the 2x counts.

The 8 ms and 1 ms CLI checks cross-checked the receive span and long-interval counts against CSV timestamps. The qualification regression also checked a fixture returning 1,250 microseconds O-to-T and 1,500 microseconds T-to-O for a 1,000-microsecond request. The thresholds follow the actual interval for each direction. The CSV comparison allows for sub-microsecond truncation at the interval boundaries.

## Physical bench acceptance

Physical-device measurements require a connected bench adapter and verified device settings. First verify the Identity Object and the intended profile, preferably input-only. Confirm output ownership and write permissions before using a profile that owns outputs, asserts Run or sends configuration bytes. Then run the Windows GUI and logger with the requested RPIs and record the Forward_Open actual intervals separately from socket spacing percentiles, longest gap, sequence gaps, timeouts, missed send slots and capture losses. Use a capture at the NIC or switch if wire-level timestamps are required.
