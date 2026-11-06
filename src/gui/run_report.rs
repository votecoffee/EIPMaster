use super::Device;
use crate::{
    model::CaptureMode,
    scanner::{CloseOutcome, Identity, ReceiveTimeout, Schedule, SendTiming, Spacing},
    settings_file,
};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use std::{net::Ipv4Addr, path::PathBuf};

#[derive(Default)]
pub(super) struct CaptureRun {
    first_generation: Option<u64>,
    last_generation: Option<u64>,
    connection: Option<ConnectionReport>,
}

impl CaptureRun {
    pub(super) fn track(&mut self, generation: u64) {
        self.first_generation.get_or_insert(generation);
        self.last_generation = Some(generation);
    }
}

#[derive(Serialize)]
struct ConnectionReport {
    generation: u64,
    state: &'static str,
    identity: Option<Identity>,
    identity_error: Option<String>,
    cip_error: Option<String>,
    network_error: Option<String>,
    adapter_ip: Ipv4Addr,
    local_ip: Ipv4Addr,
    input_assembly: u16,
    input_bytes: u16,
    output_assembly: u16,
    output_bytes: u16,
    configuration_bytes: u16,
    input_only: bool,
    input_run_idle: bool,
    output_run_idle: bool,
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
    receive_timeout: Option<ReceiveTimeout>,
    forward_close: Option<CloseOutcome>,
    sent: u64,
    skipped_deadlines: u64,
    receive_spacing: Spacing,
    send_spacing: Spacing,
    send_timing: SendTiming,
}

#[derive(Serialize)]
struct RunReport<'a> {
    mode: &'static str,
    version: &'static str,
    saved_utc: String,
    device_label: &'a str,
    capture: CaptureReport<'a>,
    connection: Option<&'a ConnectionReport>,
}

#[derive(Serialize)]
struct CaptureReport<'a> {
    started_utc: &'a str,
    csv_prefix: String,
    mode: CaptureMode,
    first_armed_generation: Option<u64>,
    last_armed_generation: Option<u64>,
    seen: u64,
    unchanged: u64,
    written: u64,
    buffer_overruns: u64,
    unconfirmed_rows: u64,
    disk_error: Option<String>,
}

impl Device {
    // Keep the final status of the latest connection during this capture.
    // Draft settings and later, unrelated connections cannot change it.
    pub(super) fn record_capture_connection(&mut self) {
        if self.capture_run.last_generation != Some(self.generation) {
            return;
        }
        let Some(active) = &self.active else {
            return;
        };
        let status = &self.status;
        let (actual_ot_us, actual_to_us) = status.actual.unzip();
        self.capture_run.connection = Some(ConnectionReport {
            generation: self.generation,
            state: status.state,
            identity: status.identity.clone(),
            identity_error: status.identity_error.clone(),
            cip_error: status.cip_error.clone(),
            network_error: status.network_error.clone(),
            adapter_ip: active.device_ip,
            local_ip: active.local_ip,
            input_assembly: active.input_instance,
            input_bytes: active.input_size,
            output_assembly: active.output_instance,
            output_bytes: active.output_size,
            configuration_bytes: active.config_size,
            input_only: active.input_only,
            input_run_idle: active.input_run_idle,
            output_run_idle: active.output_run_idle,
            schedule: self.preferences.schedule,
            requested_ot_us: active.output_rpi_us,
            requested_to_us: active.input_rpi_us,
            actual_ot_us,
            actual_to_us,
            accepted: status.valid,
            sequence_gaps: status.gaps,
            duplicates: status.duplicates,
            late: status.late,
            invalid: status.invalid,
            timeouts: status.timeouts,
            receive_timeout: status.receive_timeout,
            forward_close: status.forward_close.clone(),
            sent: self.tx,
            skipped_deadlines: self.skipped,
            receive_spacing: status.spacing,
            send_spacing: self.tx_spacing,
            send_timing: self.send_timing,
        });
    }

    pub(super) fn save_run_report(&self) -> Result<PathBuf, String> {
        use std::sync::atomic::Ordering::Relaxed;

        if self.capturing || self.link_busy() {
            return Err("Disconnect and stop capture before saving the report".into());
        }
        let capture = self.capture.as_ref().ok_or("No CSV capture to report")?;
        let sink = &capture.sink;
        let seen = sink.seen.load(Relaxed);
        let unchanged = sink.unchanged.load(Relaxed);
        let written = sink.written.load(Relaxed);
        let buffer_overruns = sink.overrun.load(Relaxed);
        let report = RunReport {
            mode: "gui",
            version: env!("CARGO_PKG_VERSION"),
            saved_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            device_label: &self.name,
            capture: CaptureReport {
                started_utc: sink.start_utc(),
                csv_prefix: sink.prefix.display().to_string(),
                mode: sink.mode(),
                first_armed_generation: self.capture_run.first_generation,
                last_armed_generation: self.capture_run.last_generation,
                seen,
                unchanged,
                written,
                buffer_overruns,
                unconfirmed_rows: seen.saturating_sub(written + unchanged + buffer_overruns),
                disk_error: sink.error.lock().unwrap().clone(),
            },
            connection: self.capture_run.connection.as_ref(),
        };
        let mut filename = sink
            .prefix
            .file_name()
            .expect("capture prefix")
            .to_os_string();
        filename.push("-report.toml");
        let path = sink.prefix.with_file_name(filename);
        let text = toml::to_string_pretty(&report)
            .map_err(|error| format!("Could not format run report — {error}"))?;
        settings_file::write_new(&path, &text)
            .map_err(|error| format!("Could not save {} — {error}", path.display()))?;
        Ok(path)
    }
}
