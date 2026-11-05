use assembly_scanner::{
    capture::{Capture, Options, Slot},
    model::Settings,
    scanner::{Connection, Identity, IdentityKey, Spacing, read_identity},
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
  eipmaster-cli identity <adapter-ipv4> <local-ipv4>
  eipmaster-cli capture <settings.toml> <new-output-dir> <seconds> <vendor:type:product>

Capture uses the deadline scheduler and requires an input-only profile.
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
    written: u64,
    buffer_overruns: u64,
    last_value: Option<String>,
    csv_prefix: String,
    identity_error: Option<String>,
    cip_error: Option<String>,
    network_error: Option<String>,
    disk_error: Option<String>,
    identity: Option<Identity>,
    receive_spacing: Spacing,
    send_spacing: Spacing,
}

fn capture(
    profile: &Path,
    directory: &Path,
    seconds: u64,
    expected: IdentityKey,
) -> Result<bool, String> {
    let text = fs::read_to_string(profile).map_err(|e| format!("{}: {e}", profile.display()))?;
    let settings: Settings = toml::from_str(&text).map_err(|e| e.to_string())?;
    settings.validate()?;
    if !settings.input_only || settings.config_size != 0 || settings.output_run_idle {
        return Err("Capture requires an input-only heartbeat with zero configuration bytes and O->T Run/Idle disabled".into());
    }
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
        Options {
            directory: PathBuf::from(directory),
            capacity: 32_768,
            rotate_bytes: 0,
            rotate_seconds: 0,
        },
    )
    .map_err(|e| e.to_string())?;
    let connection =
        match Connection::start_verified_input(settings.clone(), slot.clone(), expected) {
            Ok(connection) => connection,
            Err(error) => {
                capture.stop(&slot);
                return Err(error);
            }
        };
    let mut deadline = None;
    let mut duration_reached = false;
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
        if capture.sink.error.lock().unwrap().is_some() {
            connection.stop();
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let status = connection.status.clone();
    let tx = connection.tx.clone();
    let skipped = connection.skipped.clone();
    let tx_spacing = connection.tx_spacing.clone();
    connection.join();
    capture.stop(&slot);
    let status = status.lock().unwrap().clone();
    let (actual_ot_us, actual_to_us) = status
        .actual
        .map_or((None, None), |(ot, to)| (Some(ot), Some(to)));
    let sink = &capture.sink;
    let report = Report {
        state: status.state,
        adapter_ip: settings.device_ip,
        local_ip: settings.local_ip,
        input_assembly: settings.input_instance,
        input_size: settings.input_size,
        duration_seconds: seconds,
        duration_reached,
        requested_ot_us: settings.output_rpi_us,
        requested_to_us: settings.input_rpi_us,
        actual_ot_us,
        actual_to_us,
        accepted: status.valid,
        sequence_gaps: status.gaps,
        duplicates: status.duplicates,
        late: status.late,
        invalid: status.invalid,
        timeouts: status.timeouts,
        sent: tx.load(Relaxed),
        skipped_deadlines: skipped.load(Relaxed),
        seen: sink.seen.load(Relaxed),
        written: sink.written.load(Relaxed),
        buffer_overruns: sink.overrun.load(Relaxed),
        last_value: status.value.map(|v| v.to_string()),
        csv_prefix: sink.prefix.display().to_string(),
        identity_error: status.identity_error,
        cip_error: status.cip_error,
        network_error: status.network_error,
        disk_error: sink.error.lock().unwrap().clone(),
        identity: status.identity,
        receive_spacing: status.spacing,
        send_spacing: *tx_spacing.lock().unwrap(),
    };
    let text = toml::to_string_pretty(&report).map_err(|e| e.to_string())?;
    report_file
        .write_all(text.as_bytes())
        .and_then(|_| report_file.flush())
        .map_err(|e| e.to_string())?;
    print!("{text}");
    Ok(report.state == "Closed"
        && report.duration_reached
        && report.written != 0
        && report.written == report.accepted
        && report.disk_error.is_none())
}
