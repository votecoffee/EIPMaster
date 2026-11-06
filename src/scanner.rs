use crate::{
    capture::Slot,
    model::{self, Settings, Value},
};
use ethernetip_connections::forward_open::{
    ForwardCloseRequest, ForwardCloseResponse, ForwardOpenRequest, ForwardOpenResponse,
    NetworkConnectionParameters,
};
use ethernetip_core::{
    cip::ReplyHeader,
    cpf::{self, Envelope, Item, item_type},
    encap::{self, Command, HEADER_LEN, Header},
    error::EipError,
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    fmt,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const OT_SOCKET: u16 = 0x8000;
const TO_SOCKET: u16 = 0x8001;
type Result<T> = std::result::Result<T, EipError>;
fn protocol(message: impl Into<String>) -> EipError {
    EipError::Protocol(message.into())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Identity {
    pub vendor: u16,
    pub device_type: u16,
    pub code: u16,
    pub major: u8,
    pub minor: u8,
    pub status: u16,
    pub serial: u32,
    pub name: String,
}
pub type IdentityKey = (u16, u16, u16);

impl Identity {
    pub fn key(&self) -> IdentityKey {
        (self.vendor, self.device_type, self.code)
    }

    pub fn pin(&self) -> IdentityPin {
        IdentityPin {
            key: self.key(),
            serial: self.serial,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityPin {
    pub key: IdentityKey,
    pub serial: u32,
}

enum ExpectedIdentity {
    Model(IdentityKey),
    Device(IdentityPin),
}

impl ExpectedIdentity {
    fn mismatch(&self, actual: &Identity) -> Option<String> {
        match *self {
            Self::Model((vendor, device_type, product))
                if (vendor, device_type, product) != actual.key() =>
            {
                Some(format!(
                    "Identity mismatch. Expected vendor:type:product {vendor}:{device_type}:{product}, got {}:{}:{}",
                    actual.vendor, actual.device_type, actual.code
                ))
            }
            Self::Device(pin) if pin != actual.pin() => Some(format!(
                "Identity mismatch. Expected {:?}, serial {:08X}; got {:?}, serial {:08X}",
                pin.key,
                pin.serial,
                actual.key(),
                actual.serial
            )),
            _ => None,
        }
    }
}

const RETRY_SECONDS: [u64; 3] = [1, 2, 4];

/// A retry budget for one explicitly started input-only session.
pub struct Reconnect {
    pin: IdentityPin,
    tried: usize,
    due: Option<Instant>,
}

impl Reconnect {
    pub fn new(pin: IdentityPin) -> Self {
        Self {
            pin,
            tried: 0,
            due: None,
        }
    }

    pub fn schedule(&mut self, now: Instant) -> bool {
        self.due = RETRY_SECONDS
            .get(self.tried)
            .map(|seconds| now + Duration::from_secs(*seconds));
        self.due.is_some()
    }

    pub fn pending(&self) -> Option<(usize, Instant)> {
        self.due.map(|due| (self.tried + 1, due))
    }

    pub fn take_ready(&mut self, now: Instant) -> Option<IdentityPin> {
        if self.due.is_some_and(|due| now >= due) {
            self.due = None;
            self.tried += 1;
            Some(self.pin)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct Spacing {
    pub intervals: u64,
    pub span_us: u64,
    pub observed_packets_per_second: Option<f64>,
    pub percent_of_negotiated_rate: Option<f64>,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub intervals_at_or_over_2x_rpi: u64,
    pub intervals_at_or_over_4x_rpi: u64,
}
struct Histogram {
    bins: Vec<u64>,
    rpi_us: u64,
    count: u64,
    span: Duration,
    max: u64,
    at_or_over_2x: u64,
    at_or_over_4x: u64,
}
impl Histogram {
    // Exact microsecond buckets through 32,767 µs, then 256 buckets per doubling.
    const EXACT_BITS: u32 = 15;
    const PRECISION_BITS: u32 = 8;
    const EXACT_BINS: usize = 1 << Self::EXACT_BITS;
    const BINS_PER_OCTAVE: usize = 1 << Self::PRECISION_BITS;
    const TOTAL_BINS: usize =
        Self::EXACT_BINS + (u64::BITS - Self::EXACT_BITS) as usize * Self::BINS_PER_OCTAVE;

    fn new(actual_rpi_us: u32) -> Self {
        Self {
            bins: vec![0; Self::TOTAL_BINS],
            rpi_us: u64::from(actual_rpi_us),
            count: 0,
            span: Duration::ZERO,
            max: 0,
            at_or_over_2x: 0,
            at_or_over_4x: 0,
        }
    }
    fn bucket(us: u64) -> usize {
        if us < Self::EXACT_BINS as u64 {
            return us as usize;
        }
        let octave = us.ilog2();
        let shift = octave - Self::PRECISION_BITS;
        let offset = (us >> shift) as usize - Self::BINS_PER_OCTAVE;
        Self::EXACT_BINS + (octave - Self::EXACT_BITS) as usize * Self::BINS_PER_OCTAVE + offset
    }

    fn bucket_upper(index: usize) -> u64 {
        if index < Self::EXACT_BINS {
            return index as u64;
        }
        let coarse = index - Self::EXACT_BINS;
        let octave = Self::EXACT_BITS + (coarse / Self::BINS_PER_OCTAVE) as u32;
        let shift = octave - Self::PRECISION_BITS;
        let mantissa = (Self::BINS_PER_OCTAVE + coarse % Self::BINS_PER_OCTAVE) as u64;
        (mantissa << shift) | ((1u64 << shift) - 1)
    }

    fn add(&mut self, gap: Duration) {
        let us = gap.as_micros().min(u64::MAX.into()) as u64;
        self.bins[Self::bucket(us)] += 1;
        self.count += 1;
        self.span += gap;
        self.max = self.max.max(us);
        self.at_or_over_2x += u64::from(us >= self.rpi_us * 2);
        self.at_or_over_4x += u64::from(us >= self.rpi_us * 4);
    }
    fn snapshot(&self) -> Spacing {
        let mut values = [0; 3];
        let targets = [50, 95, 99].map(|pct| (self.count * pct).div_ceil(100));
        let mut cumulative = 0;
        let mut i = 0;
        for (index, n) in self.bins.iter().enumerate() {
            cumulative += n;
            while i < 3 && cumulative >= targets[i] {
                values[i] = Self::bucket_upper(index).min(self.max);
                i += 1;
            }
            if i == 3 {
                break;
            }
        }
        let observed_packets_per_second =
            (!self.span.is_zero()).then(|| self.count as f64 / self.span.as_secs_f64());
        let negotiated_packets_per_second = 1_000_000.0 / self.rpi_us as f64;
        Spacing {
            intervals: self.count,
            span_us: self.span.as_micros().min(u64::MAX.into()) as u64,
            observed_packets_per_second,
            percent_of_negotiated_rate: observed_packets_per_second
                .map(|rate| 100.0 * rate / negotiated_packets_per_second),
            p50_us: values[0],
            p95_us: values[1],
            p99_us: values[2],
            max_us: self.max,
            intervals_at_or_over_2x_rpi: self.at_or_over_2x,
            intervals_at_or_over_4x_rpi: self.at_or_over_4x,
        }
    }
}

/// Terminal receive timing. The silence includes the interval after the last valid packet.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct ReceiveTimeout {
    pub waiting_for_first_input: bool,
    pub limit_us: u64,
    pub valid_input_silence_us: u64,
    pub last_udp_age_us: Option<u64>,
    pub last_sequence: Option<u32>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CloseOutcome {
    Confirmed,
    Rejected {
        general_status: u8,
        extended_status: Vec<u16>,
    },
    Unconfirmed {
        error: String,
    },
}

impl CloseOutcome {
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed)
    }
}

impl fmt::Display for CloseOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Confirmed => f.write_str("confirmed"),
            Self::Rejected {
                general_status,
                extended_status,
            } => {
                write!(f, "rejected (CIP general 0x{general_status:02X}")?;
                for word in extended_status {
                    write!(f, ", extended 0x{word:04X}")?;
                }
                f.write_str(")")
            }
            Self::Unconfirmed { error } => write!(f, "unconfirmed ({error})"),
        }
    }
}

#[derive(Clone, Default)]
pub struct Status {
    pub state: &'static str,
    pub identity: Option<Identity>,
    pub identity_error: Option<String>,
    pub network_error: Option<String>,
    pub cip_error: Option<String>,
    pub actual: Option<(u32, u32)>,
    pub cyclic_io: Option<CyclicIo>,
    pub forward_open_exchange_us: Option<u64>,
    pub first_udp_after_open_us: Option<u64>,
    pub first_input_after_open_us: Option<u64>,
    pub last_input: Option<Instant>,
    pub input: Vec<u8>,
    pub value: Option<Value>,
    pub input_run: Option<bool>,
    pub valid: u64,
    pub gaps: u64,
    pub duplicates: u64,
    pub late: u64,
    pub invalid: u64,
    pub timeouts: u64,
    pub receive_timeout: Option<ReceiveTimeout>,
    pub forward_close: Option<CloseOutcome>,
    pub retryable: bool,
    pub spacing: Spacing,
}

/// Socket endpoints and connection IDs used by an accepted Forward_Open.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CyclicIo {
    pub local_udp: SocketAddr,
    pub output_destination: SocketAddr,
    pub first_input_source: Option<SocketAddr>,
    pub ot_connection_id: u32,
    pub to_connection_id: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Schedule {
    Deadline,
    DeadlineAboveNormal,
    Tokio,
}

/// Wall-clock timing measured on the cyclic producer thread.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct SendTiming {
    pub above_normal_applied: Option<bool>,
    pub max_dispatch_lateness_us: Option<u64>,
    pub max_send_call_us: u64,
    pub send_calls_at_or_over_rpi: u64,
    #[serde(skip)]
    pub first_send_after_open_us: Option<u64>,
}

/// Monotonic host timings for the explicit exchange and the start of cyclic I/O.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct StartupTiming {
    pub forward_open_exchange_us: u64,
    pub first_send_after_open_us: Option<u64>,
    pub first_udp_after_open_us: Option<u64>,
    pub first_input_after_open_us: Option<u64>,
}

impl Status {
    pub fn startup_timing(&self, send_timing: SendTiming) -> Option<StartupTiming> {
        Some(StartupTiming {
            forward_open_exchange_us: self.forward_open_exchange_us?,
            first_send_after_open_us: send_timing.first_send_after_open_us,
            first_udp_after_open_us: self.first_udp_after_open_us,
            first_input_after_open_us: self.first_input_after_open_us,
        })
    }
}

struct InputTimes {
    origin: Instant,
    first_us: AtomicU64,
    latest_us: AtomicU64,
}

impl InputTimes {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            first_us: AtomicU64::new(0),
            latest_us: AtomicU64::new(0),
        }
    }

    fn record(&self, stamp: Instant, first: bool) {
        // An offset of zero in either atomic means no input has arrived yet.
        let offset = (stamp - self.origin).as_micros() as u64 + 1;
        if first {
            self.first_us.store(offset, Relaxed);
        }
        self.latest_us.store(offset, Relaxed);
    }

    fn timestamp(&self, offset: &AtomicU64) -> Option<Instant> {
        let micros = offset.load(Relaxed).checked_sub(1)?;
        Some(self.origin + Duration::from_micros(micros))
    }
}

pub struct Connection {
    pub status: Arc<Mutex<Status>>,
    pub output: Arc<Mutex<Vec<u8>>>,
    pub tx: Arc<AtomicU64>,
    pub skipped: Arc<AtomicU64>,
    pub tx_spacing: Arc<Mutex<Spacing>>,
    pub send_timing: Arc<Mutex<SendTiming>>,
    input_times: Arc<InputTimes>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}
impl Connection {
    pub fn start(
        settings: Settings,
        slot: Slot,
        generation: u64,
        schedule: Schedule,
        permit_writes: bool,
    ) -> std::result::Result<Self, String> {
        Self::start_inner(settings, slot, generation, schedule, permit_writes, None)
    }

    pub fn start_verified_input(
        settings: Settings,
        slot: Slot,
        expected: IdentityKey,
    ) -> std::result::Result<Self, String> {
        Self::start_verified_input_with_schedule(settings, slot, expected, Schedule::Deadline)
    }

    pub fn start_verified_input_with_schedule(
        settings: Settings,
        slot: Slot,
        expected: IdentityKey,
        schedule: Schedule,
    ) -> std::result::Result<Self, String> {
        Self::start_inner(
            settings,
            slot,
            1,
            schedule,
            false,
            Some(ExpectedIdentity::Model(expected)),
        )
    }

    pub fn start_pinned_input(
        settings: Settings,
        slot: Slot,
        generation: u64,
        schedule: Schedule,
        expected: IdentityPin,
    ) -> std::result::Result<Self, String> {
        Self::start_pinned(settings, slot, generation, schedule, false, expected)
    }

    pub fn start_pinned(
        settings: Settings,
        slot: Slot,
        generation: u64,
        schedule: Schedule,
        permit_writes: bool,
        expected: IdentityPin,
    ) -> std::result::Result<Self, String> {
        Self::start_inner(
            settings,
            slot,
            generation,
            schedule,
            permit_writes,
            Some(ExpectedIdentity::Device(expected)),
        )
    }

    fn start_inner(
        settings: Settings,
        slot: Slot,
        generation: u64,
        schedule: Schedule,
        permit_writes: bool,
        expected: Option<ExpectedIdentity>,
    ) -> std::result::Result<Self, String> {
        settings.validate()?;
        if !permit_writes && !settings.input_only_without_writes() {
            return Err(
                "Confirm bench ownership, configuration and output permission first".into(),
            );
        }
        let mut shared = Shared {
            status: Arc::new(Mutex::new(Status {
                state: "Opening",
                ..Status::default()
            })),
            output: Arc::new(Mutex::new(settings.output()?)),
            stop: Arc::new(AtomicBool::new(false)),
            tx: Arc::new(AtomicU64::new(0)),
            skipped: Arc::new(AtomicU64::new(0)),
            tx_spacing: Arc::new(Mutex::new(Spacing::default())),
            send_timing: Arc::new(Mutex::new(SendTiming::default())),
            input_times: Arc::new(InputTimes::new()),
        };
        let connection = shared.clone();
        let worker = thread::Builder::new()
            .name("class1-session".into())
            .spawn(move || {
                let result = run(
                    &settings,
                    &slot,
                    generation,
                    schedule,
                    expected,
                    &mut shared,
                );
                let mut status = shared.status.lock().unwrap();
                if let Err(error) = result {
                    status.state = "Error";
                    status.retryable = matches!(error, EipError::Io(_));
                    match error {
                        EipError::Cip { status: code, ext } => {
                            status.cip_error = Some(format!(
                                "CIP general 0x{code:02X}, extended [{}]",
                                ext.iter()
                                    .map(|x| format!("0x{x:04X}"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                        }
                        other => status.network_error = Some(other.to_string()),
                    }
                } else if status.state != "Timed out" {
                    status.state = match &status.forward_close {
                        Some(close) if !close.is_confirmed() => "Close unconfirmed",
                        _ => "Closed",
                    };
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            status: connection.status,
            output: connection.output,
            stop: connection.stop,
            tx: connection.tx,
            skipped: connection.skipped,
            tx_spacing: connection.tx_spacing,
            send_timing: connection.send_timing,
            input_times: connection.input_times,
            worker,
        })
    }
    pub fn first_input_at(&self) -> Option<Instant> {
        self.input_times.timestamp(&self.input_times.first_us)
    }
    pub fn input_age(&self) -> Option<Duration> {
        self.input_times
            .timestamp(&self.input_times.latest_us)
            .map(|stamp| stamp.elapsed())
    }
    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
    }
    pub fn finished(&self) -> bool {
        self.worker.is_finished()
    }
    pub fn join(self) {
        let _ = self.worker.join();
    }
}
#[derive(Clone)]
struct Shared {
    status: Arc<Mutex<Status>>,
    output: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    tx: Arc<AtomicU64>,
    skipped: Arc<AtomicU64>,
    tx_spacing: Arc<Mutex<Spacing>>,
    send_timing: Arc<Mutex<SendTiming>>,
    input_times: Arc<InputTimes>,
}

struct Session {
    tcp: TcpStream,
    handle: u32,
    context: u64,
}
impl Session {
    fn open(cfg: &Settings) -> Result<Self> {
        let tcp = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
        tcp.bind(&SocketAddr::from((cfg.local_ip, 0)).into())?;
        tcp.connect_timeout(
            &SocketAddr::from((cfg.device_ip, 44818)).into(),
            Duration::from_secs(2),
        )?;
        let tcp: TcpStream = tcp.into();
        tcp.set_read_timeout(Some(Duration::from_secs(2)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(2)))?;
        tcp.set_nodelay(true)?;
        let mut session = Self {
            tcp,
            handle: 0,
            context: 0,
        };
        let (header, body) = session.exchange(Command::RegisterSession, &[1, 0, 0, 0])?;
        if header.session_handle == 0 || body != [1, 0, 0, 0] {
            return Err(protocol("Invalid RegisterSession reply"));
        }
        session.handle = header.session_handle;
        Ok(session)
    }
    fn exchange(&mut self, cmd: Command, payload: &[u8]) -> Result<(Header, Vec<u8>)> {
        self.context += 1;
        let context = self.context.to_le_bytes();
        self.tcp
            .write_all(&encap::encode_frame(cmd, self.handle, context, payload))?;
        let mut raw = [0; HEADER_LEN];
        self.tcp.read_exact(&mut raw)?;
        let header = Header::parse(&raw)?;
        let mut body = vec![0; header.length as usize];
        self.tcp.read_exact(&mut body)?;
        if header.command != cmd.as_u16()
            || header.sender_context != context
            || (self.handle != 0 && header.session_handle != self.handle)
            || header.status != 0
        {
            return Err(protocol(format!(
                "Invalid encapsulation reply to {cmd:?}, status 0x{:08X}",
                header.status
            )));
        }
        Ok((header, body))
    }
    fn cip(
        &mut self,
        service: u8,
        class: u8,
        body: &[u8],
        endpoint: Option<SocketAddr>,
    ) -> Result<(Vec<u8>, Envelope)> {
        let mut request = vec![service, 2, 0x20, class, 0x24, 1];
        request.extend_from_slice(body);
        let mut items = vec![
            Item::null_address(),
            Item::new(item_type::UNCONNECTED_DATA, request),
        ];
        if let Some(addr) = endpoint {
            items.push(Item::new(TO_SOCKET, cpf::encode_sockaddr_in_v4(addr)?));
        }
        let (_, reply) = self.exchange(Command::SendRRData, &cpf::encode_envelope(0, 5, &items))?;
        let envelope = Envelope::parse(&reply)?;
        let item = envelope
            .find(item_type::UNCONNECTED_DATA)
            .ok_or_else(|| protocol("No CIP reply item"))?;
        let header = ReplyHeader::parse(&item.data)?;
        if item.data[0] != (service | 0x80) {
            return Err(protocol("Unexpected CIP reply service"));
        }
        if header.general_status != 0 {
            return Err(EipError::Cip {
                status: header.general_status,
                ext: header.extended_status,
            });
        }
        Ok((item.data[header.body_offset..].to_vec(), envelope))
    }

    fn forward_close(&mut self, request: &ForwardCloseRequest) -> Result<()> {
        let (body, _) = self.cip(0x4e, 6, &request.encode(), None)?;
        let reply = ForwardCloseResponse::decode(&body)?;
        if reply.connection_serial != request.connection_serial
            || reply.originator_vendor != request.originator_vendor
            || reply.originator_serial != request.originator_serial
        {
            return Err(protocol("Mismatched Forward_Close connection identifiers"));
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.handle != 0 {
            let _ = self.tcp.write_all(&encap::encode_frame(
                Command::UnRegisterSession,
                self.handle,
                [0; 8],
                &[],
            ));
        }
    }
}
pub fn read_identity(cfg: &Settings) -> std::result::Result<Identity, String> {
    identity(&mut Session::open(cfg).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn identity(session: &mut Session) -> Result<Identity> {
    let (body, _) = session.cip(1, 1, &[], None)?;
    if body.len() < 15 || body.len() < 15 + body[14] as usize {
        return Err(protocol("Short Identity Object reply"));
    }
    Ok(Identity {
        vendor: le16(&body, 0),
        device_type: le16(&body, 2),
        code: le16(&body, 4),
        major: body[6],
        minor: body[7],
        status: le16(&body, 8),
        serial: le32(&body, 10),
        name: String::from_utf8_lossy(&body[15..15 + body[14] as usize]).into_owned(),
    })
}
fn le16(raw: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(raw[at..at + 2].try_into().unwrap())
}
fn le32(raw: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(raw[at..at + 4].try_into().unwrap())
}

fn segment(path: &mut Vec<u8>, code: u8, value: u16) {
    if let Ok(v) = u8::try_from(value) {
        path.extend([code, v]);
    } else {
        path.extend([code | 1, 0]);
        path.extend(value.to_le_bytes());
    }
}
fn path(cfg: &Settings) -> Vec<u8> {
    let mut path = vec![0x20, 4];
    if let Some(instance) = cfg.config_instance {
        segment(&mut path, 0x24, instance);
    }
    segment(&mut path, 0x2c, cfg.output_instance);
    segment(&mut path, 0x2c, cfg.input_instance);
    path
}
fn params(size: u16, header: bool, priority: u8) -> NetworkConnectionParameters {
    NetworkConnectionParameters {
        redundant_owner: false,
        connection_type: 2,
        priority,
        variable_size: false,
        size: size + 2 + 4 * u16::from(header),
    }
}
fn run(
    cfg: &Settings,
    slot: &Slot,
    generation: u64,
    schedule: Schedule,
    expected: Option<ExpectedIdentity>,
    shared: &mut Shared,
) -> Result<()> {
    let mut session = Session::open(cfg)?;
    {
        let id = identity(&mut session);
        let mut state = shared.status.lock().unwrap();
        if let Some(expected) = expected {
            match &id {
                Ok(actual) => {
                    if let Some(message) = expected.mismatch(actual) {
                        state.identity = Some(actual.clone());
                        state.identity_error = Some(message.clone());
                        return Err(protocol(message));
                    }
                }
                Err(error) => {
                    state.identity_error = Some(error.to_string());
                    return id.map(|_| ());
                }
            }
        }
        match id {
            Ok(id) => state.identity = Some(id),
            Err(e @ EipError::Cip { .. }) => state.identity_error = Some(e.to_string()),
            Err(e) => {
                state.identity_error = Some(e.to_string());
                drop(state);
                session = Session::open(cfg)?;
            }
        }
    }
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_recv_buffer_size(512 * 1024)?;
    socket.bind(&SocketAddr::from((cfg.local_ip, cfg.local_udp_port)).into())?;
    let udp: UdpSocket = socket.into();
    #[cfg(not(windows))]
    udp.set_read_timeout(Some(Duration::from_millis(2)))?;
    udp.set_write_timeout(Some(Duration::from_millis(10)))?;

    let mut ids = [0; 10];
    getrandom::fill(&mut ids).map_err(|e| protocol(e.to_string()))?;
    let close_path = path(cfg);
    let mut open_path = close_path.clone();
    let config = model::parse_hex(&cfg.config_hex).map_err(protocol)?;
    if !config.is_empty() {
        open_path.extend([0x80, config.len().div_ceil(2) as u8]);
        open_path.extend(&config);
        if config.len() % 2 != 0 {
            open_path.push(0);
        }
    }
    let open = ForwardOpenRequest {
        priority_tick: 0x0a,
        timeout_ticks: 0x0a,
        o_to_t_connection_id: 0,
        t_to_o_connection_id: le32(&ids, 0).max(1),
        connection_serial: le16(&ids, 4).max(1),
        originator_vendor: 0,
        originator_serial: le32(&ids, 6).max(1),
        connection_timeout_mult: cfg.timeout_multiplier_code,
        o_to_t_rpi_us: cfg.output_rpi_us,
        o_to_t_params: params(cfg.output_size, cfg.output_run_idle, cfg.output_priority),
        t_to_o_rpi_us: cfg.input_rpi_us,
        t_to_o_params: params(cfg.input_size, cfg.input_run_idle, cfg.input_priority),
        transport_type: 1,
        connection_path: open_path,
    };
    if shared.stop.load(Relaxed) {
        return Ok(());
    }
    let local_addr = udp.local_addr()?;
    let request = open.encode();
    let open_started = Instant::now();
    let open_result = session.cip(0x54, 6, &request, Some(local_addr));
    let open_completed = Instant::now();
    shared.status.lock().unwrap().forward_open_exchange_us =
        Some((open_completed - open_started).as_micros() as u64);
    let (body, envelope) = open_result?;
    let close = ForwardCloseRequest {
        priority_tick: open.priority_tick,
        timeout_ticks: open.timeout_ticks,
        connection_serial: open.connection_serial,
        originator_vendor: open.originator_vendor,
        originator_serial: open.originator_serial,
        connection_path: close_path,
    };
    let io_result = (|| {
        let reply = ForwardOpenResponse::decode(&body)?;
        if reply.connection_serial != open.connection_serial
            || reply.originator_vendor != open.originator_vendor
            || reply.originator_serial != open.originator_serial
            || reply.o_to_t_connection_id == 0
            || reply.t_to_o_connection_id == 0
            || reply.o_to_t_actual_rpi_us == 0
            || reply.t_to_o_actual_rpi_us == 0
        {
            return Err(protocol(
                "Forward_Open returned invalid connection identifiers or intervals",
            ));
        }
        let peer = cpf::resolve_peer_udp(&envelope, OT_SOCKET, cfg.device_ip.into(), 2222);
        if peer.ip() != cfg.device_ip {
            return Err(protocol("Adapter advertised an unexpected O->T address"));
        }
        {
            let mut status = shared.status.lock().unwrap();
            status.actual = Some((reply.o_to_t_actual_rpi_us, reply.t_to_o_actual_rpi_us));
            status.cyclic_io = Some(CyclicIo {
                local_udp: local_addr,
                output_destination: peer,
                first_input_source: None,
                ot_connection_id: reply.o_to_t_connection_id,
                to_connection_id: reply.t_to_o_connection_id,
            });
            status.state = "Waiting for input";
        }
        thread::scope(|scope| {
            let sender = scope.spawn(|| {
                let result = produce(
                    &udp,
                    peer,
                    cfg.output_run_idle,
                    &reply,
                    open_completed,
                    shared,
                    schedule,
                );
                if result.is_err() {
                    shared.stop.store(true, Relaxed);
                }
                result
            });
            let result = receive(&udp, cfg, &reply, open_completed, slot, generation, shared);
            shared.stop.store(true, Relaxed);
            let tx = sender
                .join()
                .map_err(|_| protocol("Cyclic sender panicked"))?;
            result.and(tx)
        })
    })();
    let close_result = session.forward_close(&close);
    let mut status = shared.status.lock().unwrap();
    // A watchdog timeout remains the primary result. Preserve the existing
    // retry policy for explicit rejections and transport failures at close.
    status.retryable &= close_result
        .as_ref()
        .err()
        .is_none_or(|error| matches!(error, EipError::Io(_)));
    status.forward_close = Some(match close_result {
        Ok(()) => CloseOutcome::Confirmed,
        Err(EipError::Cip { status, ext }) => CloseOutcome::Rejected {
            general_status: status,
            extended_status: ext,
        },
        Err(error) => CloseOutcome::Unconfirmed {
            error: error.to_string(),
        },
    });
    io_result
}

fn request_above_normal() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
        };
        // SAFETY: The current thread pseudo-handle stays valid for the call.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) != 0 }
    }
    #[cfg(not(windows))]
    false
}

fn publish_send_status(shared: &Shared, spacing: &Histogram, timing: SendTiming) {
    *shared.tx_spacing.lock().unwrap() = spacing.snapshot();
    *shared.send_timing.lock().unwrap() = timing;
}

fn produce(
    udp: &UdpSocket,
    peer: SocketAddr,
    header: bool,
    reply: &ForwardOpenResponse,
    open_completed: Instant,
    shared: &Shared,
    schedule: Schedule,
) -> Result<()> {
    let period = Duration::from_micros(reply.o_to_t_actual_rpi_us.into());
    let size = shared.output.lock().unwrap().len();
    let data_start = 20 + 4 * usize::from(header);
    let mut packet = vec![0; data_start + size];
    packet[0..6].copy_from_slice(&[2, 0, 2, 0x80, 8, 0]);
    packet[6..10].copy_from_slice(&reply.o_to_t_connection_id.to_le_bytes());
    packet[14..16].copy_from_slice(&0x00b1u16.to_le_bytes());
    let connected_len = (packet.len() - 18) as u16;
    packet[16..18].copy_from_slice(&connected_len.to_le_bytes());
    if header {
        packet[20] = 1;
    }
    let mut sequence = 0u32;
    let mut spacing = Histogram::new(reply.o_to_t_actual_rpi_us);
    let mut last = None;
    let mut publish = Instant::now();
    let mut timing = SendTiming {
        above_normal_applied: (schedule == Schedule::DeadlineAboveNormal)
            .then(request_above_normal),
        ..SendTiming::default()
    };
    let mut send = |lateness: Option<Duration>| -> Result<()> {
        if let Some(lateness) = lateness {
            let us = lateness.as_micros() as u64;
            timing.max_dispatch_lateness_us =
                Some(timing.max_dispatch_lateness_us.unwrap_or(0).max(us));
        }
        sequence = sequence.wrapping_add(1);
        packet[10..14].copy_from_slice(&sequence.to_le_bytes());
        packet[18..20].copy_from_slice(&(sequence as u16).to_le_bytes());
        packet[data_start..].copy_from_slice(&shared.output.lock().unwrap());
        let call_start = Instant::now();
        let result = udp.send_to(&packet, peer);
        let stamp = Instant::now();
        let call_time = stamp - call_start;
        timing.max_send_call_us = timing.max_send_call_us.max(call_time.as_micros() as u64);
        timing.send_calls_at_or_over_rpi += u64::from(call_time >= period);
        result?;
        timing
            .first_send_after_open_us
            .get_or_insert_with(|| (stamp - open_completed).as_micros() as u64);
        if let Some(prior) = last.replace(stamp) {
            spacing.add(stamp - prior);
        }
        if stamp - publish >= Duration::from_millis(100) {
            publish_send_status(shared, &spacing, timing);
            publish = stamp;
        }
        shared.tx.fetch_add(1, Relaxed);
        Ok(())
    };
    if schedule == Schedule::Tokio {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let result = runtime.block_on(async {
            let mut interval = tokio::time::interval(period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !shared.stop.load(Relaxed) {
                tokio::select! {
                    _ = interval.tick() => if !shared.stop.load(Relaxed) { send(None)?; },
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {},
                }
            }
            Ok(())
        });
        publish_send_status(shared, &spacing, timing);
        return result;
    }
    let mut waiter = crate::deadline_wait::DeadlineWait::new();
    let mut deadline = Instant::now();
    let spin = Duration::from_micros(150).min(period / 4);
    let result = loop {
        if shared.stop.load(Relaxed) {
            break Ok(());
        }
        let now = Instant::now();
        if now < deadline {
            let remaining = deadline - now;
            if remaining > spin {
                waiter.wait((remaining - spin).min(Duration::from_millis(10)));
            } else {
                std::hint::spin_loop();
            }
            continue;
        }
        let lateness = now - deadline;
        let missed = lateness.as_nanos() / period.as_nanos();
        if missed != 0 {
            shared.skipped.fetch_add(missed as u64, Relaxed);
        }
        deadline =
            now + (period - Duration::from_nanos((lateness.as_nanos() % period.as_nanos()) as u64));
        if let Err(error) = send(Some(lateness)) {
            break Err(error);
        }
    };
    publish_send_status(shared, &spacing, timing);
    result
}

fn parse_packet(
    packet: &[u8],
    connection: u32,
    header: bool,
    size: u16,
) -> Option<(u32, u16, Option<bool>, &[u8])> {
    let start = 20 + 4 * usize::from(header);
    if packet.len() != start + size as usize
        || packet[..6] != [2, 0, 2, 0x80, 8, 0]
        || le32(packet, 6) != connection
        || le16(packet, 14) != 0xb1
        || le16(packet, 16) as usize != packet.len() - 18
    {
        return None;
    }
    Some((
        le32(packet, 10),
        le16(packet, 18),
        header.then(|| le32(packet, 20) & 1 != 0),
        &packet[start..],
    ))
}
#[cfg(windows)]
fn wait_for_udp(udp: &UdpSocket) -> io::Result<bool> {
    use std::{os::windows::io::AsRawSocket, ptr::null_mut};
    use windows_sys::Win32::Networking::WinSock::{
        FD_SET, SOCKET_ERROR, TIMEVAL, WSAGetLastError, select,
    };

    let mut readable = FD_SET {
        fd_count: 1,
        ..FD_SET::default()
    };
    readable.fd_array[0] = udp.as_raw_socket() as usize;
    let timeout = TIMEVAL {
        tv_sec: 0,
        tv_usec: 2_000,
    };
    // SAFETY: The arguments stay valid for this call, and the borrowed socket stays open.
    let result = unsafe { select(0, &mut readable, null_mut(), null_mut(), &timeout) };
    if result == SOCKET_ERROR {
        Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
    } else {
        Ok(result != 0)
    }
}

fn receive_datagram(udp: &UdpSocket, packet: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
    // Wait for readiness so an idle interval never cancels a Windows recvfrom.
    #[cfg(windows)]
    if !wait_for_udp(udp)? {
        return Ok(None);
    }
    match udp.recv_from(packet) {
        Ok(packet) => Ok(Some(packet)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn receive(
    udp: &UdpSocket,
    cfg: &Settings,
    reply: &ForwardOpenResponse,
    open_completed: Instant,
    slot: &Slot,
    generation: u64,
    shared: &Shared,
) -> Result<()> {
    let started = Instant::now();
    let first_timeout = Duration::from_secs(2).max(Duration::from_micros(
        u64::from(reply.t_to_o_actual_rpi_us) * 2,
    ));
    let watchdog = Duration::from_micros(
        cfg.connection_timeout(Some(reply.t_to_o_actual_rpi_us))
            .receive_watchdog_us
            .expect("validated timeout code and actual input interval"),
    );
    let mut packet = [0; 65_536];
    let mut last_seq: Option<u32> = None;
    let mut last_udp = None;
    let mut seen = 0u64;
    let mut status = shared.status.lock().unwrap().clone();
    let mut spacing = Histogram::new(reply.t_to_o_actual_rpi_us);
    let mut publish = started;
    let result = loop {
        if shared.stop.load(Relaxed) {
            break Ok(());
        }
        let result = receive_datagram(udp, &mut packet);
        let stamp = Instant::now();
        match result {
            Ok(Some((length, addr))) => {
                if last_udp.replace(stamp).is_none() {
                    status.first_udp_after_open_us =
                        Some((stamp - open_completed).as_micros() as u64);
                }
                match parse_packet(
                    &packet[..length],
                    reply.t_to_o_connection_id,
                    cfg.input_run_idle,
                    cfg.input_size,
                )
                .filter(|_| addr.ip() == cfg.device_ip)
                {
                    Some((seq, cip, run, bytes)) => {
                        let delta = last_seq.map_or(1, |previous| seq.wrapping_sub(previous));
                        if delta == 0 {
                            status.duplicates += 1;
                        } else if delta >= 0x8000_0000 {
                            let distance = last_seq.unwrap().wrapping_sub(seq);
                            if distance < 64 && seen & (1 << distance) != 0 {
                                status.duplicates += 1;
                            } else {
                                status.late += 1;
                            }
                        } else {
                            status.gaps += u64::from(delta - 1);
                            last_seq = Some(seq);
                            seen = seen.checked_shl(delta).unwrap_or(0) | 1;
                            if let Some(last) = status.last_input {
                                spacing.add(stamp - last);
                            }
                            status.last_input = Some(stamp);
                            let first = status.valid == 0;
                            if first {
                                status.first_input_after_open_us =
                                    Some((stamp - open_completed).as_micros() as u64);
                                if let Some(cyclic_io) = &mut status.cyclic_io {
                                    cyclic_io.first_input_source = Some(addr);
                                }
                            }
                            shared.input_times.record(stamp, first);
                            status.state = "Receiving";
                            status.valid += 1;
                            status.input.clear();
                            status.input.extend_from_slice(bytes);
                            status.input_run = run;
                            if let Some(sink) = &*slot.read().unwrap() {
                                sink.record(stamp, generation, seq, cip, bytes);
                            }
                        }
                    }
                    None => status.invalid += 1,
                }
            }
            Ok(None) => {}
            Err(e) => break Err(e.into()),
        }
        let (since, limit) = status
            .last_input
            .map_or((started, first_timeout), |last| (last, watchdog));
        let silence = stamp - since;
        if silence >= limit {
            status.receive_timeout = Some(ReceiveTimeout {
                waiting_for_first_input: status.last_input.is_none(),
                limit_us: limit.as_micros() as u64,
                valid_input_silence_us: silence.as_micros() as u64,
                last_udp_age_us: last_udp.map(|last| (stamp - last).as_micros() as u64),
                last_sequence: last_seq,
            });
            status.timeouts += 1;
            status.retryable = true;
            status.state = "Timed out";
            break Ok(());
        }
        if stamp - publish >= Duration::from_millis(100) {
            status.spacing = spacing.snapshot();
            publish_receive_status(shared, &mut status, cfg.selection);
            publish = stamp;
        }
    };
    status.spacing = spacing.snapshot();
    publish_receive_status(shared, &mut status, cfg.selection);
    result
}

fn publish_receive_status(shared: &Shared, status: &mut Status, selection: model::Selection) {
    status.value = selection.decode(&status.input).map(Value::into_owned);
    let mut latest = shared.status.lock().unwrap();
    status.identity.clone_from(&latest.identity);
    status.identity_error.clone_from(&latest.identity_error);
    latest.clone_from(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacing_reports_short_intervals_exactly_and_bounds_long_percentiles() {
        let mut histogram = Histogram::new(1_000);
        for (us, count) in [(1_001, 50), (30_000, 45), (2_000_000, 4), (5_000_000, 1)] {
            for _ in 0..count {
                histogram.add(Duration::from_micros(us));
            }
        }
        let summary = histogram.snapshot();
        assert_eq!(summary.intervals, 100);
        assert_eq!(summary.p50_us, 1_001);
        assert_eq!(summary.p95_us, 30_000);
        assert!((2_000_000..=2_008_000).contains(&summary.p99_us));
        assert_eq!(summary.max_us, 5_000_000);

        let mut edge = Histogram::new(1_000);
        edge.add(Duration::from_micros(32_768));
        assert_eq!(edge.snapshot().p99_us, 32_768);
        edge.add(Duration::from_micros(u64::MAX));
        let summary = edge.snapshot();
        assert_eq!(summary.p99_us, u64::MAX);
        assert_eq!(summary.max_us, u64::MAX);
    }

    #[test]
    fn spacing_counts_long_intervals_against_the_actual_rpi_and_preserves_span() {
        let mut histogram = Histogram::new(1_250);
        assert_eq!(histogram.snapshot().span_us, 0);
        for ns in [2_499_999, 2_500_000, 4_999_999, 5_000_000] {
            histogram.add(Duration::from_nanos(ns));
        }
        let summary = histogram.snapshot();
        assert_eq!(summary.intervals, 4);
        assert_eq!(summary.span_us, 14_999);
        assert_eq!(summary.intervals_at_or_over_2x_rpi, 3);
        assert_eq!(summary.intervals_at_or_over_4x_rpi, 1);
    }

    #[test]
    fn packet_rate_measures_interval_count_over_elapsed_span() {
        let mut histogram = Histogram::new(1_000);
        assert!(histogram.snapshot().observed_packets_per_second.is_none());
        histogram.add(Duration::ZERO);
        assert!(histogram.snapshot().percent_of_negotiated_rate.is_none());
        for us in [1_000, 1_000, 6_000] {
            histogram.add(Duration::from_micros(us));
        }
        let summary = histogram.snapshot();
        assert_eq!(summary.span_us, 8_000);
        assert!((summary.observed_packets_per_second.unwrap() - 500.0).abs() < 1e-9);
        assert!((summary.percent_of_negotiated_rate.unwrap() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn input_clock_preserves_the_first_packet_as_the_latest_advances() {
        let times = InputTimes::new();
        assert!(times.timestamp(&times.first_us).is_none());
        let first = times.origin + Duration::from_micros(800);
        let latest = times.origin + Duration::from_micros(1_800);
        times.record(first, true);
        times.record(latest, false);
        assert_eq!(times.timestamp(&times.first_us), Some(first));
        assert_eq!(times.timestamp(&times.latest_us), Some(latest));
    }

    #[test]
    fn reconnect_backoff_is_bounded_to_three_attempts() {
        let pin = IdentityPin {
            key: (1, 2, 3),
            serial: 4,
        };
        let mut reconnect = Reconnect::new(pin);
        let mut now = Instant::now();
        for (attempt, seconds) in [1, 2, 4].into_iter().enumerate() {
            assert!(reconnect.schedule(now));
            let due = now + Duration::from_secs(seconds);
            assert_eq!(reconnect.pending(), Some((attempt + 1, due)));
            assert_eq!(reconnect.take_ready(due - Duration::from_millis(1)), None);
            assert_eq!(reconnect.take_ready(due), Some(pin));
            assert_eq!(reconnect.take_ready(due), None);
            now = due;
        }
        assert!(!reconnect.schedule(now));
        assert_eq!(reconnect.pending(), None);
    }
}
