use super::{ExpectedIdentity, Report, capture, load_capture_profile, report_text};
use assembly_scanner::{model::CaptureMode, settings_file};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use std::{ffi::OsString, fs, net::Ipv4Addr, path::Path};

const BASELINE_US: u32 = 8_000;
const TARGET_US: u32 = 1_000;
const RUNS: [&str; 4] = ["baseline-8ms", "trial-1", "trial-2", "trial-3"];

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LinkPath {
    Direct,
    Switched,
    Loopback,
}

#[derive(Serialize)]
struct Qualification {
    mode: &'static str,
    link_path: LinkPath,
    version: &'static str,
    started_utc: String,
    finished_utc: Option<String>,
    adapter_ip: Ipv4Addr,
    local_ip: Ipv4Addr,
    expected_identity: String,
    planned_runs: [&'static str; 4],
    baseline_seconds: u64,
    target_seconds: u64,
    outcome: &'static str,
    current_run: Option<&'static str>,
    runs: Vec<Trial>,
}

#[derive(Serialize)]
struct Trial {
    name: &'static str,
    errors: Vec<String>,
    capture: Option<Report>,
}

fn now_utc() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn save_summary(summary: &Qualification, path: &Path) -> Result<String, String> {
    let text = report_text(summary)?;
    settings_file::replace(path, &text)
        .map_err(|error| format!("Cannot save {}: {error}", path.display()))?;
    Ok(text)
}

fn intervals_match(report: &Report, requested_us: u32) -> bool {
    report.actual_ot_us == Some(requested_us) && report.actual_to_us == Some(requested_us)
}

fn trial_errors(report: &Report, requested_us: u32) -> Vec<String> {
    let mut errors = Vec::new();
    if !report.completed() {
        errors.push("Capture did not complete cleanly".into());
    }
    if !intervals_match(report, requested_us) {
        errors
            .push("Forward_Open intervals differ from the requested RPI or are unavailable".into());
    }
    for (counter, name) in [
        (report.sequence_gaps, "receive sequence gaps"),
        (report.duplicates, "duplicate packets"),
        (report.late, "late packets"),
        (report.invalid, "invalid packets"),
        (report.timeouts, "receive timeouts"),
        (report.buffer_overruns, "CSV buffer overruns"),
    ] {
        if counter != 0 {
            errors.push(format!("{counter} {name}"));
        }
    }
    if report.seen != report.accepted || report.written != report.accepted || report.unchanged != 0
    {
        errors.push("CSV row counts differ from accepted packets".into());
    }
    errors
}

pub(super) fn run(
    profile: &Path,
    directory: &Path,
    path: &str,
    expected: ExpectedIdentity,
    seconds: &[OsString],
) -> Result<bool, String> {
    let serial = expected
        .serial
        .ok_or("Qualification requires decimal vendor:type:product:serial")?;
    let link_path = match path {
        "direct" => LinkPath::Direct,
        "switched" => LinkPath::Switched,
        "loopback" => LinkPath::Loopback,
        _ => return Err("Select direct, switched, or loopback".into()),
    };
    let (baseline_seconds, target_seconds) = match seconds {
        [] => (15, 60),
        [value] if link_path == LinkPath::Loopback => {
            let seconds = value
                .to_string_lossy()
                .parse::<u64>()
                .ok()
                .filter(|value| (1..=60).contains(value))
                .ok_or("Loopback seconds must be an integer from 1 to 60")?;
            (seconds, seconds)
        }
        _ => return Err("A shorter duration is available only for loopback qualification".into()),
    };
    let (mut settings, mut preferences) = load_capture_profile(profile)?;
    let loopback = link_path == LinkPath::Loopback;
    if settings.device_ip.is_loopback() != loopback || settings.local_ip.is_loopback() != loopback {
        return Err("Loopback qualification requires loopback adapter and NIC addresses. Direct and switched runs require non-loopback addresses.".into());
    }

    settings.capture_mode = CaptureMode::EveryPacket;
    preferences.auto_reconnect = false;
    preferences.directory = directory.display().to_string();
    let mut baseline = settings.clone();
    baseline.input_rpi_us = BASELINE_US;
    baseline.output_rpi_us = BASELINE_US;
    settings.input_rpi_us = TARGET_US;
    settings.output_rpi_us = TARGET_US;
    let baseline_profile = settings_file::profile_text(&baseline, &preferences)?;
    let target_profile = settings_file::profile_text(&settings, &preferences)?;

    fs::create_dir(directory).map_err(|error| {
        format!(
            "Cannot create new qualification directory {}: {error}",
            directory.display()
        )
    })?;
    for (name, content) in [
        ("baseline-profile.toml", baseline_profile),
        ("target-profile.toml", target_profile),
    ] {
        let path = directory.join(name);
        settings_file::write_new(&path, &content)
            .map_err(|error| format!("Cannot save {}: {error}", path.display()))?;
    }
    let receipt = directory.join("qualification.toml");
    let (vendor, device_type, product) = expected.key;
    let mut summary = Qualification {
        mode: "headless",
        link_path,
        version: env!("CARGO_PKG_VERSION"),
        started_utc: now_utc(),
        finished_utc: None,
        adapter_ip: settings.device_ip,
        local_ip: settings.local_ip,
        expected_identity: format!("{vendor}:{device_type}:{product}:{serial}"),
        planned_runs: RUNS,
        baseline_seconds,
        target_seconds,
        outcome: "running",
        current_run: None,
        runs: Vec::with_capacity(RUNS.len()),
    };

    for (index, name) in RUNS.into_iter().enumerate() {
        let (settings, seconds) = if index == 0 {
            (&baseline, baseline_seconds)
        } else {
            (&settings, target_seconds)
        };
        summary.current_run = Some(name);
        save_summary(&summary, &receipt)?;
        let mut trial = Trial {
            name,
            errors: Vec::new(),
            capture: None,
        };
        let blocking = match capture(
            settings,
            &preferences,
            &directory.join(name),
            seconds,
            expected,
        ) {
            Ok(report) => {
                trial.errors = trial_errors(&report, settings.input_rpi_us);
                let stop = report.identity_error.is_some()
                    || report.disk_error.is_some()
                    || (index == 0
                        && !(report.completed() && intervals_match(&report, BASELINE_US)));
                trial.capture = Some(report);
                stop
            }
            Err(error) => {
                trial.errors.push(error);
                true
            }
        };
        summary.current_run = None;
        summary.runs.push(trial);
        save_summary(&summary, &receipt)?;
        if blocking {
            break;
        }
    }

    let passed =
        summary.runs.len() == RUNS.len() && summary.runs.iter().all(|run| run.errors.is_empty());
    summary.outcome = if passed { "passed" } else { "failed" };
    summary.finished_utc = Some(now_utc());
    println!("{}", save_summary(&summary, &receipt)?);
    Ok(passed)
}
