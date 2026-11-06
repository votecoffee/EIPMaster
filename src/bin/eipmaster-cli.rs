use assembly_scanner::{
    capture::{Capture, Slot},
    eds,
    model::Settings,
    preflight,
    scanner::{
        Connection, Identity, IdentityKey, Reconnect, Schedule, Spacing, Status, read_identity,
    },
    settings_file,
};
use serde::Serialize;
use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::Ordering::Relaxed,
    thread,
    time::{Duration, Instant},
};

const USAGE: &str = "\
Usage:
  eipmaster-cli eds <vendor.eds>
  eipmaster-cli validate <settings.toml> [<vendor.eds> <ConnectionN>]
  eipmaster-cli identity <adapter-ipv4> <local-ipv4>
  eipmaster-cli capture <settings.toml> <new-output-dir> <seconds> <vendor:type:product>

Capture requires an input-only profile. The scheduler defaults to deadline. [app].auto_reconnect opts in to input-only recovery.
The expected Identity Object values are decimal and checked before Forward_Open.
Seconds must be an integer from 1 to 3600.";

fn main() -> ExitCode {
    match run(&env::args_os().skip(1).collect::<Vec<_>>()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[OsString]) -> Result<bool, String> {
    match args {
        [command, path] if command == "eds" => {
            let result = EdsReport {
                profiles: eds::load(Path::new(path))?
                    .into_iter()
                    .map(|p| {
                        let assembly_definition_sizes = ["O->T size", "T->O size"]
                            .into_iter()
                            .filter(|field| p.size_from_assembly_definition(field))
                            .collect();
                        EdsProfile {
                            id: p.id,
                            name: p.name,
                            supported: p.unsupported.is_empty(),
                            unsupported: p.unsupported,
                            assembly_definition_sizes,
                            fields: p.fields,
                        }
                    })
                    .collect(),
            };
            print!(
                "{}",
                toml::to_string_pretty(&result).map_err(|e| e.to_string())?
            );
            Ok(true)
        }
        [command, path, comparison @ ..] if command == "validate" => {
            let (settings, preferences) = settings_file::load_profile(Path::new(path))?;
            let profile = match comparison {
                [] => None,
                [eds_path, id] => Some(
                    eds::load(Path::new(eds_path))?
                        .into_iter()
                        .find(|profile| profile.id.eq_ignore_ascii_case(&id.to_string_lossy()))
                        .ok_or_else(|| format!("EDS profile {:?} was not found", id))?,
                ),
                _ => return Err(USAGE.into()),
            };
            let report = preflight::inspect(
                &settings,
                &preferences.unwrap_or_default(),
                profile.as_ref(),
            );
            print!(
                "{}",
                toml::to_string_pretty(&report).map_err(|e| e.to_string())?
            );
            Ok(report.connection_valid)
        }
        [command, adapter, local] if command == "identity" => {
            let settings = Settings {
                device_ip: adapter
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| "Invalid adapter IPv4 address")?,
                local_ip: local
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| "Invalid local IPv4 address")?,
                ..Settings::default()
            };
            settings.validate()?;
            let result = IdentityReport {
                adapter_ip: settings.device_ip,
                local_ip: settings.local_ip,
                identity: read_identity(&settings)?,
            };
            print!(
                "{}",
                toml::to_string_pretty(&result).map_err(|e| e.to_string())?
            );
            Ok(true)
        }
        [command, profile, directory, seconds, expected] if command == "capture" => {
            let seconds = seconds
                .to_string_lossy()
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=3600).contains(n))
                .ok_or("Seconds must be an integer from 1 to 3600")?;
            let expected = parse_identity_key(&expected.to_string_lossy())?;
            capture(Path::new(profile), Path::new(directory), seconds, expected)
        }
        [] => {
            println!("{USAGE}");
            Ok(true)
        }
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{USAGE}");
            Ok(true)
        }
        _ => Err(USAGE.into()),
    }
}

fn parse_identity_key(value: &str) -> Result<IdentityKey, String> {
    let mut numbers = value.split(':').map(str::parse::<u16>);
    match (
        numbers.next(),
        numbers.next(),
        numbers.next(),
        numbers.next(),
    ) {
        (Some(Ok(vendor)), Some(Ok(device_type)), Some(Ok(product)), None) => {
            Ok((vendor, device_type, product))
        }
        _ => Err("Expected decimal vendor:type:product".into()),
    }
}

#[derive(Serialize)]
struct EdsReport {
    profiles: Vec<EdsProfile>,
}

#[derive(Serialize)]
struct EdsProfile {
    id: String,
    name: String,
    supported: bool,
    unsupported: Vec<String>,
    assembly_definition_sizes: Vec<&'static str>,
    fields: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct IdentityReport {
    adapter_ip: std::net::Ipv4Addr,
    local_ip: std::net::Ipv4Addr,
    identity: Identity,
}

#[derive(Serialize)]
struct Report {
    state: &'static str,
    adapter_ip: std::net::Ipv4Addr,
    local_ip: std::net::Ipv4Addr,
    input_assembly: u16,
    input_size: u16,
    duration_seconds: u64,
    duration_reached: bool,
    schedule: Schedule,
    requested_ot_us: u32,
    requested_to_us: u32,
    actual_ot_us: Option<u32>,
    actual_to_us: Option<u32>,
    accepted: u64,
    sequence_gaps: u64,
    duplicates: u64,
    late: u64,
    invalid: u64,
    timeouts: u64,
    sent: u64,
    skipped_deadlines: u64,
    seen: u64,
    unchanged: u64,
    written: u64,
    buffer_overruns: u64,
    last_value: Option<String>,
    last_input_hex: Option<String>,
    csv_prefix: String,
    identity_error: Option<String>,
    cip_error: Option<String>,
    network_error: Option<String>,
    disk_error: Option<String>,
    identity: Option<Identity>,
    receive_spacing: Spacing,
    send_spacing: Spacing,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attempts: Vec<AttemptReport>,
}

#[derive(Serialize)]
struct AttemptReport {
    generation: usize,
    state: &'static str,
    actual_ot_us: Option<u32>,
    actual_to_us: Option<u32>,
    accepted: u64,
    sequence_gaps: u64,
    timeouts: u64,
    identity_error: Option<String>,
    cip_error: Option<String>,
    network_error: Option<String>,
    receive_spacing: Spacing,
    send_spacing: Spacing,
}

#[derive(Default)]
struct Attempt {
    status: Status,
    sent: u64,
    skipped: u64,
    send_spacing: Spacing,
}

impl Attempt {
    fn finish(connection: Connection) -> Self {
        let status = connection.status.clone();
        let tx = connection.tx.clone();
        let skipped = connection.skipped.clone();
        let send_spacing = connection.tx_spacing.clone();
        connection.join();
        let status = status.lock().unwrap().clone();
        let send_spacing = *send_spacing.lock().unwrap();
        Self {
            status,
            sent: tx.load(Relaxed),
            skipped: skipped.load(Relaxed),
            send_spacing,
        }
    }

    fn report(&self, generation: usize) -> AttemptReport {
        let status = &self.status;
        let (actual_ot_us, actual_to_us) = status.actual.unzip();
        AttemptReport {
            generation,
            state: status.state,
            actual_ot_us,
            actual_to_us,
            accepted: status.valid,
            sequence_gaps: status.gaps,
            timeouts: status.timeouts,
            identity_error: status.identity_error.clone(),
            cip_error: status.cip_error.clone(),
            network_error: status.network_error.clone(),
            receive_spacing: status.spacing,
            send_spacing: self.send_spacing,
        }
    }
}

fn capture(
    profile: &Path,
    directory: &Path,
    seconds: u64,
    expected: IdentityKey,
) -> Result<bool, String> {
    let (settings, preferences) = settings_file::load_profile(profile)?;
    let preferences = preferences.unwrap_or_default();
    settings.validate()?;
    if !settings.input_only_without_writes() {
        return Err("Capture requires an input-only heartbeat with zero configuration bytes and O->T Run/Idle disabled".into());
    }
    settings.csv_fields()?;
    fs::create_dir(directory).map_err(|e| {
        format!(
            "Cannot create new capture directory {}: {e}",
            directory.display()
        )
    })?;
    let mut report_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("capture.toml"))
        .map_err(|e| e.to_string())?;
    let slot = Slot::default();
    let mut capture = Capture::start(
        &slot,
        preferences.capture_options(PathBuf::from(directory)),
        &settings,
    )
    .map_err(|e| e.to_string())?;
    let sink = capture.sink.clone();
    let mut attempts = Vec::new();
    let mut reconnect = None;
    let mut pin = None;
    let mut deadline = None;
    let mut duration_reached = false;
    'capture: loop {
        let connection = match pin {
            Some(pin) => Connection::start_pinned_input(
                settings.clone(),
                slot.clone(),
                attempts.len() as u64 + 1,
                preferences.schedule,
                pin,
            ),
            None => Connection::start_verified_input_with_schedule(
                settings.clone(),
                slot.clone(),
                expected,
                preferences.schedule,
            ),
        };
        let connection = match connection {
            Ok(connection) => connection,
            Err(error) if !attempts.is_empty() => {
                attempts.push(Attempt {
                    status: Status {
                        state: "Error",
                        network_error: Some(error),
                        ..Status::default()
                    },
                    ..Attempt::default()
                });
                break;
            }
            Err(error) => {
                capture.stop(&slot);
                return Err(error);
            }
        };
        while !connection.finished() {
            let now = Instant::now();
            if deadline.is_none()
                && let Some(age) = connection.input_age()
            {
                deadline = Some(now - age + Duration::from_secs(seconds));
            }
            if deadline.is_some_and(|end| now >= end) {
                duration_reached = true;
                connection.stop();
                break;
            }
            if capture.finished() {
                connection.stop();
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let attempt = Attempt::finish(connection);
        if preferences.auto_reconnect
            && attempts.is_empty()
            && attempt.status.identity_error.is_none()
        {
            reconnect = attempt
                .status
                .identity
                .as_ref()
                .map(|id| Reconnect::new(id.pin()));
        }
        let retryable = attempt.status.retryable;
        attempts.push(attempt);
        if duration_reached || capture.finished() || !retryable {
            break;
        }
        let Some(reconnect) = reconnect.as_mut() else {
            break;
        };
        if !reconnect.schedule(Instant::now()) {
            break;
        }
        loop {
            let now = Instant::now();
            if deadline.is_some_and(|end| now >= end) {
                duration_reached = true;
                break 'capture;
            }
            if capture.finished() {
                break 'capture;
            }
            if let Some(next) = reconnect.take_ready(now) {
                pin = Some(next);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    capture.stop(&slot);
    let last = attempts.last().unwrap();
    let status = &last.status;
    let last_input = attempts
        .iter()
        .rev()
        .find(|run| run.status.valid != 0)
        .map(|run| &run.status);
    let (actual_ot_us, actual_to_us) = status.actual.unzip();
    let sum = |field: fn(&Attempt) -> u64| attempts.iter().map(field).sum();
    let report = Report {
        state: status.state,
        adapter_ip: settings.device_ip,
        local_ip: settings.local_ip,
        input_assembly: settings.input_instance,
        input_size: settings.input_size,
        duration_seconds: seconds,
        duration_reached,
        schedule: preferences.schedule,
        requested_ot_us: settings.output_rpi_us,
        requested_to_us: settings.input_rpi_us,
        actual_ot_us,
        actual_to_us,
        accepted: sum(|run| run.status.valid),
        sequence_gaps: sum(|run| run.status.gaps),
        duplicates: sum(|run| run.status.duplicates),
        late: sum(|run| run.status.late),
        invalid: sum(|run| run.status.invalid),
        timeouts: sum(|run| run.status.timeouts),
        sent: sum(|run| run.sent),
        skipped_deadlines: sum(|run| run.skipped),
        seen: sink.seen.load(Relaxed),
        unchanged: sink.unchanged.load(Relaxed),
        written: sink.written.load(Relaxed),
        buffer_overruns: sink.overrun.load(Relaxed),
        last_value: last_input
            .and_then(|run| run.value.as_ref())
            .map(ToString::to_string),
        last_input_hex: last_input.map(|run| assembly_scanner::model::hex(&run.input)),
        csv_prefix: sink.prefix.display().to_string(),
        identity_error: status.identity_error.clone(),
        cip_error: status.cip_error.clone(),
        network_error: status.network_error.clone(),
        disk_error: sink.error.lock().unwrap().clone(),
        identity: status.identity.clone(),
        receive_spacing: status.spacing,
        send_spacing: last.send_spacing,
        attempts: if preferences.auto_reconnect {
            attempts
                .iter()
                .enumerate()
                .map(|(index, run)| run.report(index + 1))
                .collect()
        } else {
            Vec::new()
        },
    };
    let text = toml::to_string_pretty(&report).map_err(|e| e.to_string())?;
    report_file
        .write_all(text.as_bytes())
        .and_then(|_| report_file.flush())
        .map_err(|e| e.to_string())?;
    print!("{text}");
    Ok(report.state == "Closed"
        && report.duration_reached
        && status.valid != 0
        && report.written != 0
        && report.written + report.unchanged == report.accepted
        && report.disk_error.is_none())
}
