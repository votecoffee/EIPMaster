use crate::{
    capture::Slot,
    model::{self, Settings, Value},
};
use ethernetip_connections::forward_open::{
    ForwardCloseRequest, ForwardOpenRequest, ForwardOpenResponse, NetworkConnectionParameters,
};
use ethernetip_core::{
    cip::ReplyHeader,
    cpf::{self, Envelope, Item, item_type},
    encap::{self, Command, HEADER_LEN, Header},
    error::EipError,
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
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
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct Spacing {
    pub intervals: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}
struct Histogram {
    bins: Vec<u64>,
    count: u64,
    max: u64,
}
impl Histogram {
    fn new() -> Self {
        Self {
            bins: vec![0; 20_001],
            count: 0,
            max: 0,
        }
    }
    fn add(&mut self, gap: Duration) {
        let us = gap.as_micros() as u64;
        self.bins[us.min(20_000) as usize] += 1;
        self.count += 1;
        self.max = self.max.max(us);
    }
    fn snapshot(&self) -> Spacing {
        let mut values = [0; 3];
        let targets = [50, 95, 99].map(|pct| (self.count * pct).div_ceil(100));
        let mut cumulative = 0;
        let mut i = 0;
        for (us, n) in self.bins.iter().enumerate() {
            cumulative += n;
            while i < 3 && cumulative >= targets[i] {
                values[i] = us as u64;
                i += 1;
            }
            if i == 3 {
                break;
            }
        }
        Spacing {
            intervals: self.count,
            p50_us: values[0],
            p95_us: values[1],
            p99_us: values[2],
            max_us: self.max,
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
    pub spacing: Spacing,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Schedule {
    Deadline,
    Tokio,
}

pub struct Connection {
    pub status: Arc<Mutex<Status>>,
    pub output: Arc<Mutex<Vec<u8>>>,
    pub tx: Arc<AtomicU64>,
    pub skipped: Arc<AtomicU64>,
    pub tx_spacing: Arc<Mutex<Spacing>>,
    clock_origin: Instant,
    latest_input_us: Arc<AtomicU64>,
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
        Self::start_inner(settings, slot, 1, Schedule::Deadline, false, Some(expected))
    }

    fn start_inner(
        settings: Settings,
        slot: Slot,
        generation: u64,
        schedule: Schedule,
        permit_writes: bool,
        expected: Option<IdentityKey>,
    ) -> std::result::Result<Self, String> {
        settings.validate()?;
        if !permit_writes
            && (!settings.input_only || settings.config_size != 0 || settings.output_run_idle)
        {
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
            clock_origin: Instant::now(),
            latest_input_us: Arc::new(AtomicU64::new(0)),
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
                    status.state = "Closed";
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
            clock_origin: connection.clock_origin,
            latest_input_us: connection.latest_input_us,
            worker,
        })
    }
    pub fn input_age(&self) -> Option<Duration> {
        let offset = self.latest_input_us.load(Relaxed).checked_sub(1)?;
        Some((self.clock_origin + Duration::from_micros(offset)).elapsed())
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
    clock_origin: Instant,
    latest_input_us: Arc<AtomicU64>,
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
    expected: Option<IdentityKey>,
    shared: &mut Shared,
) -> Result<()> {
    let mut session = Session::open(cfg)?;
    {
        let id = identity(&mut session);
        let mut state = shared.status.lock().unwrap();
        if let Some(expected) = expected {
            match id {
                Ok(ref actual) if actual.key() != expected => {
                    let message = format!(
                        "Identity mismatch. Expected vendor:type:product {}:{}:{}, got {}:{}:{}",
                        expected.0,
                        expected.1,
                        expected.2,
                        actual.vendor,
                        actual.device_type,
                        actual.code
                    );
                    state.identity = Some(actual.clone());
                    state.identity_error = Some(message.clone());
                    return Err(protocol(message));
                }
                Err(ref error) => {
                    state.identity_error = Some(error.to_string());
                    return Err(protocol("Identity verification failed before Forward_Open"));
                }
                _ => {}
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
        connection_timeout_mult: 3,
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
    let (body, envelope) = session.cip(0x54, 6, &open.encode(), Some(udp.local_addr()?))?;
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
            status.state = "Waiting for input";
        }
        thread::scope(|scope| {
            let sender = scope.spawn(|| {
                let result = produce(&udp, peer, cfg.output_run_idle, &reply, shared, schedule);
                if result.is_err() {
                    shared.stop.store(true, Relaxed);
                }
                result
            });
            let result = receive(&udp, cfg, &reply, slot, generation, shared);
            shared.stop.store(true, Relaxed);
            let tx = sender
                .join()
                .map_err(|_| protocol("Cyclic sender panicked"))?;
            result.and(tx)
        })
    })();
    let close_result = session.cip(0x4e, 6, &close.encode(), None);
    io_result?;
    close_result?;
    Ok(())
}

fn produce(
    udp: &UdpSocket,
    peer: SocketAddr,
    header: bool,
    reply: &ForwardOpenResponse,
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
    let mut spacing = Histogram::new();
    let mut last = None;
    let mut publish = Instant::now();
    let mut send = || -> Result<()> {
        sequence = sequence.wrapping_add(1);
        packet[10..14].copy_from_slice(&sequence.to_le_bytes());
        packet[18..20].copy_from_slice(&(sequence as u16).to_le_bytes());
        packet[data_start..].copy_from_slice(&shared.output.lock().unwrap());
        udp.send_to(&packet, peer)?;
        let stamp = Instant::now();
        if let Some(prior) = last.replace(stamp) {
            spacing.add(stamp - prior);
        }
        if stamp - publish >= Duration::from_millis(100) {
            *shared.tx_spacing.lock().unwrap() = spacing.snapshot();
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
                    _ = interval.tick() => if !shared.stop.load(Relaxed) { send()?; },
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {},
                }
            }
            Ok(())
        });
        *shared.tx_spacing.lock().unwrap() = spacing.snapshot();
        return result;
    }
    let mut deadline = Instant::now();
    let spin = Duration::from_micros(150).min(period / 4);
    while !shared.stop.load(Relaxed) {
        let now = Instant::now();
        if now < deadline {
            let remaining = deadline - now;
            if remaining > spin {
                thread::sleep((remaining - spin).min(Duration::from_millis(10)));
            } else {
                std::hint::spin_loop();
            }
            continue;
        }
        let missed = (now - deadline).as_nanos() / period.as_nanos();
        if missed != 0 {
            shared.skipped.fetch_add(missed as u64, Relaxed);
        }
        deadline = now
            + (period
                - Duration::from_nanos(((now - deadline).as_nanos() % period.as_nanos()) as u64));
        send()?;
    }
    *shared.tx_spacing.lock().unwrap() = spacing.snapshot();
    Ok(())
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
fn receive(
    udp: &UdpSocket,
    cfg: &Settings,
    reply: &ForwardOpenResponse,
    slot: &Slot,
    generation: u64,
    shared: &Shared,
) -> Result<()> {
    let started = Instant::now();
    let first_timeout = Duration::from_secs(2).max(Duration::from_micros(
        u64::from(reply.t_to_o_actual_rpi_us) * 2,
    ));
    let watchdog = Duration::from_micros(u64::from(reply.t_to_o_actual_rpi_us) * 32);
    let mut packet = [0; 65_536];
    let mut last_seq: Option<u32> = None;
    let mut seen = 0u64;
    let mut status = shared.status.lock().unwrap().clone();
    let mut spacing = Histogram::new();
    let mut publish = started;
    let result = loop {
        if shared.stop.load(Relaxed) {
            break Ok(());
        }
        let result = udp.recv_from(&mut packet);
        let stamp = Instant::now();
        match result {
            Ok((length, addr)) => {
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
                            shared.latest_input_us.store(
                                (stamp - shared.clock_origin).as_micros() as u64 + 1,
                                Relaxed,
                            );
                            status.state = "Receiving";
                            status.valid += 1;
                            status.input.clear();
                            status.input.extend_from_slice(bytes);
                            status.input_run = run;
                            status.value = cfg.selection.decode(bytes);
                            if let Some(value) = status.value {
                                if let Some(sink) = &*slot.read().unwrap() {
                                    sink.record(stamp, generation, seq, cip, value);
                                }
                            }
                        }
                    }
                    None => status.invalid += 1,
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => break Err(e.into()),
        }
        if status
            .last_input
            .map_or(stamp - started >= first_timeout, |last| {
                stamp - last >= watchdog
            })
        {
            status.timeouts += 1;
            status.state = "Timed out";
            break Ok(());
        }
        if stamp - publish >= Duration::from_millis(100) {
            status.spacing = spacing.snapshot();
            publish_receive_status(shared, &mut status);
            publish = stamp;
        }
    };
    status.spacing = spacing.snapshot();
    publish_receive_status(shared, &mut status);
    result
}

fn publish_receive_status(shared: &Shared, status: &mut Status) {
    let mut latest = shared.status.lock().unwrap();
    status.identity.clone_from(&latest.identity);
    status.identity_error.clone_from(&latest.identity_error);
    latest.clone_from(status);
}
