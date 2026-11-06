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

Machine-readable results are under `evidence/` in the release package.

## Physical bench acceptance

Physical-device measurements require a connected bench adapter and verified device settings. First verify the Identity Object and the intended profile, preferably input-only. Confirm output ownership and write permissions before using a profile that owns outputs, asserts Run or sends configuration bytes. Then run the Windows GUI and logger with the requested RPIs and record the Forward_Open actual intervals separately from socket spacing percentiles, longest gap, sequence gaps, timeouts, missed send slots and capture losses. Use a capture at the NIC or switch if wire-level timestamps are required.
