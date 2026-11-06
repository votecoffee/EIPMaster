use crate::{
    capture::{self, Capture},
    eds::{self, Profile},
    model::{self, ByteSwap, CaptureField, CaptureMode, Kind, Selection, Settings},
    preflight,
    scanner::{
        self, Connection, Identity, IdentityPin, Reconnect, Schedule, SendTiming, Spacing, Status,
    },
    settings_file::{self, NamedDevice, Preferences, Session, Workspace},
};
use eframe::egui::{self, Color32, RichText, Ui};
use std::{
    fs,
    io::ErrorKind,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{atomic::Ordering::Relaxed, mpsc},
    time::{Duration, Instant},
};

mod run_report;

const CYAN: Color32 = Color32::from_rgb(99, 226, 213);
const AMBER: Color32 = Color32::from_rgb(247, 191, 101);
const RED: Color32 = Color32::from_rgb(255, 119, 119);

pub fn run() -> eframe::Result {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let benchmarks = if args.len() == 3 && args[0] == "--loopback-benchmark" && args[1] == "dual" {
        let root = PathBuf::from(&args[2]);
        [
            ("healthy", Ipv4Addr::LOCALHOST, 8_000),
            ("stalled", Ipv4Addr::new(127, 0, 0, 2), 16_000),
        ]
        .into_iter()
        .map(|(name, address, rpi)| Benchmark {
            schedule: Schedule::Deadline,
            directory: root.join(name),
            started: Instant::now(),
            stopping: false,
            owner: false,
            profile: None,
            reconnect: false,
            transitioned: false,
            target: Some((address, rpi)),
        })
        .collect()
    } else if matches!(args.len(), 3 | 4) && args[0] == "--loopback-benchmark" {
        let schedule = if args[1] == "tokio" {
            Schedule::Tokio
        } else {
            Schedule::Deadline
        };
        vec![Benchmark {
            schedule,
            directory: PathBuf::from(&args[2]),
            started: Instant::now(),
            stopping: false,
            owner: args.len() == 3 && args[1] == "owner",
            profile: args.get(3).map(PathBuf::from),
            reconnect: args[1] == "reconnect",
            transitioned: false,
            target: None,
        }]
    } else {
        Vec::new()
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 860.0])
            .with_min_inner_size([900.0, 580.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Assembly Scanner",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            let mut style = (*cc.egui_ctx.style()).clone();
            for font in style.text_styles.values_mut() {
                font.size += 1.0;
            }
            style.visuals = egui::Visuals::dark();
            style.visuals.panel_fill = Color32::from_rgb(22, 28, 37);
            style.visuals.window_fill = Color32::from_rgb(28, 36, 46);
            style.visuals.selection.bg_fill = Color32::from_rgb(22, 83, 87);
            style.spacing.item_spacing = egui::vec2(9.0, 7.0);
            cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
            Ok(Box::new(App::new(benchmarks, Workspace::path())))
        }),
    )
}

struct Benchmark {
    schedule: Schedule,
    directory: PathBuf,
    started: Instant,
    stopping: bool,
    owner: bool,
    profile: Option<PathBuf>,
    reconnect: bool,
    transitioned: bool,
    target: Option<(Ipv4Addr, u32)>,
}

struct RememberedSession {
    path: PathBuf,
    saved: Workspace,
    check_at: Instant,
    error: Option<String>,
}

struct Device {
    id: u64,
    name: String,
    settings: Settings,
    device: String,
    local: String,
    eds_path: String,
    toml_path: String,
    replace_profile: Option<PathBuf>,
    profiles: Vec<Profile>,
    selected: Option<usize>,
    bench_confirmed: bool,
    writes_confirmed: bool,
    output_entries: Vec<String>,
    preview_hex: String,
    connection: Option<Connection>,
    active: Option<Settings>,
    status: Status,
    generation: u64,
    tx: u64,
    skipped: u64,
    tx_spacing: Spacing,
    send_timing: SendTiming,
    capture: Option<Capture>,
    capture_run: run_report::CaptureRun,
    slot: capture::Slot,
    capturing: bool,
    preferences: Preferences,
    notice: Option<String>,
    error: Option<String>,
    identity_task: Option<mpsc::Receiver<Result<Identity, String>>>,
    verified_identity: Option<IdentityPin>,
    reconnect: Option<Reconnect>,
    bench: Option<Benchmark>,
}
impl Device {
    fn new(id: u64, bench: Option<Benchmark>) -> Self {
        let mut device = Self {
            id,
            name: format!("Device {id}"),
            settings: Settings::default(),
            device: "127.0.0.1".into(),
            local: "127.0.0.1".into(),
            eds_path: "samples/loopback.eds".into(),
            toml_path: String::new(),
            replace_profile: None,
            profiles: vec![],
            selected: None,
            bench_confirmed: false,
            writes_confirmed: false,
            output_entries: Vec::new(),
            preview_hex: String::new(),
            connection: None,
            active: None,
            status: Status::default(),
            generation: 0,
            tx: 0,
            skipped: 0,
            tx_spacing: Spacing::default(),
            send_timing: SendTiming::default(),
            capture: None,
            capture_run: run_report::CaptureRun::default(),
            slot: Default::default(),
            capturing: false,
            preferences: Preferences::default(),
            notice: None,
            error: None,
            identity_task: None,
            verified_identity: None,
            reconnect: None,
            bench,
        };
        if let Some(bench) = &device.bench {
            device.preferences.directory = bench.directory.to_string_lossy().into_owned();
            device.preferences.schedule = bench.schedule;
            device.preferences.rotate_seconds = 3;
            if let Some(profile) = &bench.profile {
                match settings_file::load_profile(profile) {
                    Ok((settings, _)) => device.settings = settings,
                    Err(error) => {
                        device.error = Some(format!("Could not load loopback settings: {error}"));
                        return device;
                    }
                }
            }
            if let Some((target, rpi)) = bench.target {
                device.device = target.to_string();
                device.settings.input_rpi_us = rpi;
                device.settings.output_rpi_us = rpi;
            }
            if bench.owner {
                device.settings.input_only = false;
                device.settings.output_instance = 100;
                device.settings.output_size = 2;
                device.settings.output_run_idle = true;
                device.settings.input_run_idle = true;
                device.settings.output_hex = "00 00".into();
                device.settings.output_fields = vec![CaptureField {
                    name: "test_word".into(),
                    selection: Selection {
                        kind: Kind::U16,
                        big_endian: true,
                        ..Selection::default()
                    },
                }];
                device
                    .settings
                    .stage_output(0, "4660")
                    .expect("fixture output mapping");
                device.settings.config_size = 3;
                device.settings.config_hex = "AA BB CC".into();
                device.writes_confirmed = true;
            }
            device.start_capture();
            device.connect(&[]);
        }
        device
    }

    fn restore(id: u64, saved: NamedDevice) -> Self {
        let mut device = Self::new(id, None);
        device.name = saved.name;
        let saved = saved.setup;
        device.settings = saved.settings;
        device.preferences = saved.preferences;
        device.device = saved.device;
        device.local = saved.local;
        device.eds_path = saved.eds_path;
        device.toml_path = saved.profile_path;
        device.profiles = saved.eds_profiles;
        device.selected = saved.selected_eds;
        device
    }

    fn snapshot(&self) -> NamedDevice {
        NamedDevice {
            name: self.name.clone(),
            setup: Session {
                settings: self.settings.clone(),
                preferences: self.preferences.clone(),
                device: self.device.clone(),
                local: self.local.clone(),
                eds_path: self.eds_path.clone(),
                profile_path: self.toml_path.clone(),
                eds_profiles: self.profiles.clone(),
                selected_eds: self.selected,
            },
        }
    }

    fn state_label(&self) -> &str {
        if self.reconnect_pending().is_some() {
            "Reconnect pending"
        } else if self.status.state.is_empty() {
            "Offline"
        } else {
            self.status.state
        }
    }

    fn reconnect_pending(&self) -> Option<(usize, Instant)> {
        self.reconnect.as_ref().and_then(Reconnect::pending)
    }

    fn link_busy(&self) -> bool {
        self.connection.is_some() || self.reconnect_pending().is_some()
    }

    fn needs_repaint(&self) -> bool {
        self.link_busy() || self.capturing || self.bench.is_some() || self.identity_task.is_some()
    }

    fn load_profile(&mut self) {
        self.replace_profile = None;
        self.notice = None;
        match settings_file::load_profile(Path::new(&self.toml_path)) {
            Ok((settings, preferences)) => {
                self.reset_endpoint_status();
                self.device = settings.device_ip.to_string();
                self.local = settings.local_ip.to_string();
                self.settings = settings;
                if let Some(preferences) = preferences {
                    self.preferences = preferences;
                }
                self.selected = None;
                self.output_entries.clear();
                self.preview_hex.clear();
                self.bench_confirmed = false;
                self.writes_confirmed = false;
                self.error = None;
                self.notice = Some("Loaded settings".into());
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn save_profile(&mut self, confirmed_path: Option<PathBuf>) {
        let replace = confirmed_path.is_some();
        let path = confirmed_path.unwrap_or_else(|| PathBuf::from(&self.toml_path));
        self.replace_profile = None;
        self.notice = None;
        if self
            .selected
            .is_some_and(|index| !self.profiles[index].unsupported.is_empty())
        {
            self.error =
                Some("Resolve the listed EDS profile issues in Manual mode before saving".into());
            return;
        }
        let text = match self
            .current()
            .and_then(|settings| settings_file::profile_text(&settings, &self.preferences))
        {
            Ok(text) => text,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let result = if replace {
            settings_file::replace(&path, &text)
        } else {
            settings_file::write_new(&path, &text)
        };
        match result {
            Ok(()) => {
                self.error = None;
                self.notice = Some(format!("Saved settings to {}", path.display()));
            }
            Err(error) if !replace && error.kind() == ErrorKind::AlreadyExists => {
                self.error = None;
                self.replace_profile = Some(path);
            }
            Err(error) => self.error = Some(format!("Could not save {} — {error}", path.display())),
        }
    }
    fn reset_endpoint_status(&mut self) {
        self.verified_identity = None;
        self.identity_task = None;
        self.active = None;
        self.status = Status::default();
        self.tx = 0;
        self.skipped = 0;
        self.tx_spacing = Spacing::default();
        self.send_timing = SendTiming::default();
    }

    fn addresses(&self) -> Result<(Ipv4Addr, Ipv4Addr), String> {
        Ok((
            self.device
                .trim()
                .parse()
                .map_err(|_| "Enter a valid adapter IPv4 address")?,
            self.local
                .trim()
                .parse()
                .map_err(|_| "Enter a valid local NIC IPv4 address")?,
        ))
    }
    fn current(&self) -> Result<Settings, String> {
        let mut s = self.settings.clone();
        (s.device_ip, s.local_ip) = self.addresses()?;
        Ok(s)
    }

    fn begin_identity_read(&mut self) {
        if self.link_busy() || self.identity_task.is_some() {
            return;
        }
        match self.current() {
            Ok(cfg) => {
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(scanner::read_identity(&cfg));
                });
                self.identity_task = Some(rx);
                self.bench_confirmed = false;
                self.writes_confirmed = false;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn connect(&mut self, reserved_ports: &[(Ipv4Addr, u16)]) {
        self.error = None;
        self.notice = None;
        let result = (|| {
            if self.identity_task.is_some() {
                return Err("Wait for the identity read to finish".into());
            }
            let s = self.current()?;
            if s.local_udp_port != 0 && reserved_ports.contains(&(s.local_ip, s.local_udp_port)) {
                return Err(format!(
                    "UDP {}:{} is already reserved by another device. Choose port 0 or another supported bind endpoint.",
                    s.local_ip, s.local_udp_port
                ));
            }
            let physical = !(s.device_ip.is_loopback() && s.local_ip.is_loopback());
            if physical && !self.bench_confirmed {
                return Err("Confirm the isolated bench connection".into());
            }
            if let Some(i) = self.selected {
                if !self.profiles[i].unsupported.is_empty() {
                    return Err(
                        "Resolve the listed profile issues in Manual mode before connecting".into(),
                    );
                }
            }
            let auto_reconnect = self.preferences.auto_reconnect && s.input_only_without_writes();
            let pin = self.verified_identity;
            if auto_reconnect && pin.is_none() {
                return Err(
                    "Use Read identity and verify the adapter before enabling automatic reconnect"
                        .into(),
                );
            }
            self.open_connection(s, pin, self.writes_confirmed)?;
            self.reconnect = pin.filter(|_| auto_reconnect).map(Reconnect::new);
            Ok(())
        })();
        self.bench_confirmed = false;
        self.writes_confirmed = false;
        if let Err(e) = result {
            self.error = Some(e);
        }
    }
    fn open_connection(
        &mut self,
        settings: Settings,
        pin: Option<IdentityPin>,
        permit_writes: bool,
    ) -> Result<(), String> {
        let generation = self.generation + 1;
        let schedule = self.preferences.schedule;
        let connection = if let Some(pin) = pin {
            Connection::start_pinned(
                settings.clone(),
                self.slot.clone(),
                generation,
                schedule,
                permit_writes,
                pin,
            )
        } else {
            Connection::start(
                settings.clone(),
                self.slot.clone(),
                generation,
                schedule,
                permit_writes,
            )
        }?;
        self.generation = generation;
        if self.capturing {
            self.capture_run.track(generation);
        }
        self.active = Some(settings);
        self.status = Status::default();
        self.tx = 0;
        self.skipped = 0;
        self.tx_spacing = Spacing::default();
        self.send_timing = SendTiming::default();
        self.identity_task = None;
        self.connection = Some(connection);
        Ok(())
    }

    fn disconnect(&mut self) {
        self.reconnect = None;
        self.notice = None;
        if let Some(connection) = &self.connection {
            connection.stop();
        }
    }

    fn start_capture(&mut self) {
        let settings = self
            .active
            .as_ref()
            .filter(|_| self.link_busy())
            .unwrap_or(&self.settings);
        let options = capture::Options {
            duration_seconds: self.preferences.gui_capture_seconds,
            ..self
                .preferences
                .capture_options(PathBuf::from(&self.preferences.directory))
        };
        match Capture::start(&self.slot, options, settings) {
            Ok(capture) => {
                self.capture = Some(capture);
                self.capture_run = run_report::CaptureRun::default();
                if self.connection.is_some() {
                    self.capture_run.track(self.generation);
                }
                self.capturing = true;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Could not start CSV capture — {e}")),
        }
    }
    fn stop_capture(&mut self) {
        if let Some(capture) = self.capture.as_mut() {
            capture.stop(&self.slot);
        }
        self.capturing = false;
    }
    fn poll(&mut self) {
        if let Some(c) = &self.connection {
            self.status = c.status.lock().unwrap().clone();
            let finished = c.finished();
            self.tx = c.tx.load(Relaxed);
            self.skipped = c.skipped.load(Relaxed);
            self.tx_spacing = *c.tx_spacing.lock().unwrap();
            self.send_timing = *c.send_timing.lock().unwrap();
            if finished {
                let c = self.connection.take().unwrap();
                self.status = c.status.lock().unwrap().clone();
                c.join();
                self.record_capture_connection();
                self.writes_confirmed = false;
                if let Some(reconnect) = &mut self.reconnect {
                    if !self.status.retryable {
                        self.reconnect = None;
                    } else if !reconnect.schedule(Instant::now()) {
                        self.reconnect = None;
                        self.error = Some("Automatic reconnect stopped after three retries".into());
                    }
                }
            }
        }
        if self.capturing
            && let Some(capture) = &self.capture
        {
            if capture.finished() {
                self.stop_capture();
            } else if capture
                .sink
                .deadline()
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.capture_run.duration_reached = true;
                self.stop_capture();
            }
        }
        if let Some((attempt, _)) = self.reconnect_pending()
            && let Some(pin) = self.reconnect.as_mut().unwrap().take_ready(Instant::now())
        {
            let settings = self.active.as_ref().expect("reconnect settings").clone();
            match self.open_connection(settings, Some(pin), false) {
                Ok(()) => self.notice = Some(format!("Automatic reconnect attempt {attempt} of 3")),
                Err(error) => {
                    self.reconnect = None;
                    self.error = Some(error);
                }
            }
        }
        if let Some(task) = &self.identity_task {
            match task.try_recv() {
                Ok(result) => {
                    match result {
                        Ok(identity) => {
                            self.verified_identity = Some(identity.pin());
                            self.bench_confirmed = false;
                            self.writes_confirmed = false;
                            self.status.identity = Some(identity);
                            self.status.identity_error = None;
                        }
                        Err(error) => {
                            self.status.identity = None;
                            self.status.identity_error = Some(error);
                        }
                    }
                    self.identity_task = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.identity_task = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn benchmark(&mut self) {
        let Some(bench) = &mut self.bench else {
            return;
        };
        if bench.started.elapsed() >= Duration::from_secs(4) && !bench.transitioned {
            if bench.owner && self.status.state == "Receiving" {
                if let Some(c) = &self.connection {
                    self.settings
                        .stage_output(0, "22136")
                        .expect("fixture output mapping");
                    *c.output.lock().unwrap() =
                        self.settings.output().expect("fixture output bytes");
                }
                bench.transitioned = true;
            } else if bench.reconnect && self.generation == 1 {
                if let Some(c) = &self.connection {
                    c.stop();
                    return;
                }
                bench.transitioned = true;
                self.connect(&[]);
                return;
            }
        }
        if !bench.stopping
            && (bench.started.elapsed() >= Duration::from_secs(12) || self.connection.is_none())
        {
            bench.stopping = true;
            if let Some(c) = &self.connection {
                c.stop();
            }
        }
        if !bench.stopping || self.connection.is_some() {
            return;
        }
        let directory = bench.directory.clone();
        self.stop_capture();
        let spacing = toml::to_string(&self.status.spacing).unwrap_or_default();
        let tx_spacing = toml::to_string(&self.tx_spacing).unwrap_or_default();
        let send_timing = toml::to_string(&self.send_timing).unwrap_or_default();
        let cip_error = self.status.cip_error.as_deref().unwrap_or_default();
        let network_error = self.status.network_error.as_deref().unwrap_or_default();
        let (ot, to) = self.status.actual.unwrap_or_default();
        let (requested_ot, requested_to) = self.active.as_ref().map_or(
            (self.settings.output_rpi_us, self.settings.input_rpi_us),
            |s| (s.output_rpi_us, s.input_rpi_us),
        );
        let setup_error = self.error.as_deref().unwrap_or_default();
        let identity_serial = self.status.identity.as_ref().map_or(0, |id| id.serial);
        let adapter_ip = self
            .active
            .as_ref()
            .map_or(self.device.as_str().to_string(), |cfg| {
                cfg.device_ip.to_string()
            });
        let (seen, unchanged, written, overrun, disk_error) = self
            .capture
            .as_ref()
            .map(|capture| {
                let s = &capture.sink;
                (
                    s.seen.load(Relaxed),
                    s.unchanged.load(Relaxed),
                    s.written.load(Relaxed),
                    s.overrun.load(Relaxed),
                    s.error.lock().unwrap().clone(),
                )
            })
            .unwrap_or_default();
        let text = format!(
            "state = {:?}\nschedule = {:?}\nadapter_ip = {adapter_ip:?}\nidentity_serial = {identity_serial}\nrequested_ot_us = {requested_ot}\nrequested_to_us = {requested_to}\nactual_ot_us = {ot}\nactual_to_us = {to}\nvalid = {}\ngaps = {}\nduplicates = {}\nlate = {}\ninvalid = {}\ntimeouts = {}\ntx = {}\nskipped_deadlines = {}\nseen = {seen}\nunchanged = {unchanged}\nwritten = {written}\noverruns = {overrun}\ndisk_error = {}\ngenerations = {}\ncip_error = {cip_error:?}\nnetwork_error = {network_error:?}\nsetup_error = {setup_error:?}\n\n[spacing]\n{spacing}\n[tx_spacing]\n{tx_spacing}\n[send_timing]\n{send_timing}",
            self.status.state,
            format!("{:?}", self.preferences.schedule),
            self.status.valid,
            self.status.gaps,
            self.status.duplicates,
            self.status.late,
            self.status.invalid,
            self.status.timeouts,
            self.tx,
            self.skipped,
            disk_error.is_some(),
            self.generation
        );
        let _ = fs::write(directory.join("scanner.toml"), text);
        self.bench = None;
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Some(c) = self.connection.take() {
            c.stop();
            c.join();
        }
        self.stop_capture();
    }
}
impl Device {
    fn show(&mut self, ctx: &egui::Context, reserved_ports: &[(Ipv4Addr, u16)]) {
        let online = self.connection.is_some();
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("ASSEMBLY SCANNER")
                        .size(23.0)
                        .strong()
                        .color(CYAN),
                );
                ui.separator();
                ui.label("EtherNet/IP  /  Class 1 cyclic I/O");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(self.state_label()).strong().color(
                        if self.status.network_error.is_some() || self.status.cip_error.is_some() {
                            RED
                        } else if online {
                            CYAN
                        } else {
                            AMBER
                        },
                    ));
                });
            });
            ui.add_space(8.0);
        });
        egui::SidePanel::left("setup")
            .resizable(true)
            .default_width(480.0)
            .min_width(380.0)
            .show(ctx, |ui| {
                ui.push_id(self.id, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.setup(ui, reserved_ports));
                });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.push_id(self.id, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.dashboard(ui));
            });
        });
    }
}

struct App {
    devices: Vec<Device>,
    selected: usize,
    next_id: u64,
    remove_confirmed: bool,
    session: Option<RememberedSession>,
}

impl App {
    fn new(benchmarks: Vec<Benchmark>, session_path: Option<PathBuf>) -> Self {
        let mut app = Self {
            devices: Vec::new(),
            selected: 0,
            next_id: 1,
            remove_confirmed: false,
            session: None,
        };
        if !benchmarks.is_empty() {
            for bench in benchmarks {
                app.devices.push(Device::new(app.next_id, Some(bench)));
                app.next_id += 1;
            }
            return app;
        }
        if let Some(path) = session_path {
            let error = match Workspace::load(&path) {
                Ok(Some(saved)) => {
                    app.selected = saved.selected_device;
                    for device in saved.devices {
                        app.devices.push(Device::restore(app.next_id, device));
                        app.next_id += 1;
                    }
                    None
                }
                Ok(None) => None,
                Err(error) => Some(format!("Could not restore last session — {error}")),
            };
            if app.devices.is_empty() {
                app.add_device();
            }
            app.session = Some(RememberedSession {
                path,
                saved: app.snapshot(),
                check_at: Instant::now(),
                error,
            });
        } else {
            app.add_device();
            app.devices[0].error = Some(
                "LOCALAPPDATA is unavailable. Use named TOML profiles to save settings.".into(),
            );
        }
        app
    }

    fn add_device(&mut self) {
        self.devices.push(Device::new(self.next_id, None));
        self.next_id += 1;
        self.selected = self.devices.len() - 1;
        self.remove_confirmed = false;
    }

    fn snapshot(&self) -> Workspace {
        Workspace {
            selected_device: self.selected,
            devices: self.devices.iter().map(Device::snapshot).collect(),
        }
    }

    fn save_session(&mut self) {
        if self.session.is_none() {
            return;
        }
        let draft = self.snapshot();
        let session = self.session.as_mut().unwrap();
        session.check_at = Instant::now() + Duration::from_secs(1);
        if draft != session.saved {
            match draft.save(&session.path) {
                Ok(()) => {
                    session.saved = draft;
                    session.error = None;
                }
                Err(error) => {
                    session.error = Some(format!("Could not save last session — {error}"));
                }
            }
        }
    }

    fn device_list(&mut self, ui: &mut Ui) {
        let prior = self.selected;
        ui.horizontal_wrapped(|ui| {
            ui.strong("Devices");
            egui::ComboBox::from_id_salt("selected-device")
                .selected_text(self.devices[self.selected].list_label())
                .show_ui(ui, |ui| {
                    for (index, device) in self.devices.iter().enumerate() {
                        ui.selectable_value(&mut self.selected, index, device.list_label());
                    }
                });
            if self.selected != prior {
                self.remove_confirmed = false;
            }
            if ui.button("Add device").clicked() {
                self.add_device();
            }
            let can_remove = self.devices.len() > 1 && !self.devices[self.selected].needs_repaint();
            let device = &mut self.devices[self.selected];
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut device.name).id_salt(device.id).desired_width(140.0));
            if ui
                .add_enabled(can_remove, egui::Button::new("Remove device"))
                .on_disabled_hover_text("Disconnect and stop capture before removing a device. Keep at least one device.")
                .clicked()
            {
                self.remove_confirmed = true;
            }
            self.remove_confirmed &= can_remove;
        });
        if self.remove_confirmed {
            ui.horizontal_wrapped(|ui| {
                ui.label("Remove this setup from the saved device list?");
                if ui.button("Remove setup").clicked() {
                    self.devices.remove(self.selected);
                    self.selected = self.selected.min(self.devices.len() - 1);
                    self.remove_confirmed = false;
                } else if ui.button("Cancel").clicked() {
                    self.remove_confirmed = false;
                }
            });
        }
        if let Some(session) = &self.session {
            if let Some(error) = &session.error {
                ui.colored_label(AMBER, error);
            } else {
                ui.small("Device setups are saved automatically")
                    .on_hover_text(session.path.display().to_string());
            }
        }
    }

    fn reserved_ports(&self) -> Vec<(Ipv4Addr, u16)> {
        self.devices
            .iter()
            .enumerate()
            .filter(|(index, device)| *index != self.selected && device.link_busy())
            .filter_map(|(_, device)| device.active.as_ref())
            .filter(|settings| settings.local_udp_port != 0)
            .map(|settings| (settings.local_ip, settings.local_udp_port))
            .collect()
    }
}

impl Device {
    fn list_label(&self) -> String {
        let name = if self.name.trim().is_empty() {
            "Unnamed device"
        } else {
            &self.name
        };
        let capture = if self.capturing { " · CSV armed" } else { "" };
        format!("{name} · {} · {}{capture}", self.device, self.state_label())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        for device in &self.devices {
            if let Some(connection) = &device.connection {
                connection.stop();
            }
        }
        self.save_session();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let benchmarking = self.devices.iter().any(|device| device.bench.is_some());
        for device in &mut self.devices {
            device.poll();
            device.benchmark();
        }
        if benchmarking && self.devices.iter().all(|device| device.bench.is_none()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::TopBottomPanel::top("devices").show(ctx, |ui| self.device_list(ui));
        let reserved_ports = self.reserved_ports();
        self.devices[self.selected].show(ctx, &reserved_ports);
        if self
            .session
            .as_ref()
            .is_some_and(|session| Instant::now() >= session.check_at)
        {
            self.save_session();
        }
        if let Some(session) = &self.session {
            ctx.request_repaint_after(session.check_at.saturating_duration_since(Instant::now()));
        }
        if self.devices.iter().any(Device::needs_repaint) {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}

fn title(ui: &mut Ui, number: &str, title: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(number).monospace().color(CYAN));
        ui.heading(title);
    });
}
fn number<T: egui::emath::Numeric>(ui: &mut Ui, label: &str, value: &mut T) {
    ui.label(label);
    ui.add(egui::DragValue::new(value).speed(0.25));
    ui.end_row();
}

fn edit_selection(ui: &mut Ui, selection: &mut Selection) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Byte offset");
        ui.add(egui::DragValue::new(&mut selection.offset));
        egui::ComboBox::from_id_salt("field-kind")
            .selected_text(format!("{:?}", selection.kind))
            .show_ui(ui, |ui| {
                for kind in Kind::ALL {
                    ui.selectable_value(&mut selection.kind, kind, format!("{kind:?}"));
                }
            });
        if selection.kind == Kind::Utf8 {
            selection.bit = None;
            selection.swap = ByteSwap::None;
            ui.label("Text bytes");
            ui.add(
                egui::DragValue::new(selection.string_length.get_or_insert(1))
                    .range(1..=model::MAX_ASSEMBLY_BYTES),
            );
        } else {
            selection.string_length = None;
            let mut bit = selection.bit.is_some();
            if ui.checkbox(&mut bit, "Single bit").changed() {
                selection.bit = bit.then_some(0);
            }
            if let Some(bit) = &mut selection.bit {
                selection.swap = ByteSwap::None;
                ui.add(egui::DragValue::new(bit).range(0..=7));
            } else if !matches!(selection.kind, Kind::Bool | Kind::U8 | Kind::I8) {
                let wide = !matches!(selection.kind, Kind::U16 | Kind::I16);
                if !wide && selection.swap == ByteSwap::WordOrder {
                    selection.swap = ByteSwap::None;
                }
                ui.checkbox(&mut selection.big_endian, "Big endian");
                egui::ComboBox::from_id_salt("field-swap")
                    .selected_text(selection.swap.label())
                    .show_ui(ui, |ui| {
                        for swap in ByteSwap::ALL {
                            if wide || swap != ByteSwap::WordOrder {
                                ui.selectable_value(&mut selection.swap, swap, swap.label());
                            }
                        }
                    });
            } else {
                selection.swap = ByteSwap::None;
            }
        }
    });
}

fn edit_fields(
    ui: &mut Ui,
    direction: &str,
    fields: &mut Vec<CaptureField>,
    mut logged: Option<&mut Vec<String>>,
    size: u16,
) -> bool {
    let mut remove = None;
    for (index, field) in fields.iter_mut().enumerate() {
        ui.push_id((direction, index), |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                let previous = field.name.clone();
                if ui.text_edit_singleline(&mut field.name).changed()
                    && let Some(names) = logged.as_deref_mut()
                {
                    for name in names.iter_mut().filter(|name| **name == previous) {
                        name.clone_from(&field.name);
                    }
                }
                if let Some(names) = logged.as_deref_mut() {
                    let mut included = names.contains(&field.name);
                    if ui.checkbox(&mut included, "CSV").changed() {
                        if included {
                            names.push(field.name.clone());
                        } else {
                            names.retain(|name| name != &field.name);
                        }
                    }
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
            edit_selection(ui, &mut field.selection);
            if let Err(error) = field.selection.validate(usize::from(size)) {
                ui.colored_label(RED, error);
            }
            ui.separator();
        });
    }
    if let Some(index) = remove {
        let field = fields.remove(index);
        if let Some(names) = logged {
            names.retain(|name| name != &field.name);
        }
        return true;
    }
    if fields.len() < model::MAX_MAPPED_FIELDS
        && ui.button(format!("Add {direction} field")).clicked()
    {
        let name = (1..)
            .map(|n| format!("{direction}_{n}"))
            .find(|name| {
                !fields
                    .iter()
                    .any(|field| field.name.eq_ignore_ascii_case(name))
            })
            .unwrap();
        fields.push(CaptureField {
            name,
            selection: Selection {
                kind: Kind::U8,
                ..Selection::default()
            },
        });
        return true;
    }
    false
}

impl Device {
    fn setup(&mut self, ui: &mut Ui, reserved_ports: &[(Ipv4Addr, u16)]) {
        let online = self.link_busy();
        title(ui, "01", "Device setup");
        ui.add_enabled_ui(!online && !self.capturing, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Settings TOML");
                if ui.add(egui::TextEdit::singleline(&mut self.toml_path).desired_width(200.0).hint_text("Path to a TOML profile")).changed() {
                    self.replace_profile = None;
                }
                let named = !self.toml_path.trim().is_empty();
                if ui.add_enabled(named, egui::Button::new("Load")).clicked() {
                    self.load_profile();
                }
                if ui.add_enabled(named, egui::Button::new("Save")).clicked() {
                    self.save_profile(None);
                }
            });
            if let Some(path) = self.replace_profile.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(AMBER, format!("Replace {}?", path.display()));
                    if ui.button("Replace profile").clicked() {
                        self.save_profile(Some(path));
                    } else if ui.button("Cancel").clicked() {
                        self.replace_profile = None;
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label("EDS file");
                ui.text_edit_singleline(&mut self.eds_path);
                if ui.button("Import").clicked() {
                    match eds::load(Path::new(&self.eds_path)) {
                        Ok(profiles) => {
                            self.profiles = profiles;
                            self.select_profile(Some(self.profiles.iter().position(|p| p.settings.input_only && p.unsupported.is_empty()).unwrap_or(0)));
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            });
            let mut chosen = self.selected;
            egui::ComboBox::from_id_salt("eds-profiles")
                .selected_text(chosen.map_or_else(|| "Manual settings".into(), |i| format!("{} · {}", self.profiles[i].id, self.profiles[i].name)))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut chosen, None, "Manual settings");
                    for (i, p) in self.profiles.iter().enumerate() {
                        ui.selectable_value(&mut chosen, Some(i), format!("{} · {}", p.id, p.name));
                    }
                });
            if chosen != self.selected { self.select_profile(chosen); }
            if let Some(i) = self.selected {
                let p = &self.profiles[i];
                ui.collapsing("EDS source values and fields to review", |ui| {
                    for (key, value) in &p.fields {
                        ui.label(format!("{key} = {value}"));
                        if p.size_from_assembly_definition(key) {
                            ui.colored_label(AMBER, "Assembly definition width. Verify the configured device payload.");
                        }
                    }
                    for key in ["O->T RPI", "T->O RPI", "T->O size"] {
                        if !p.fields.contains_key(key) { ui.colored_label(AMBER, format!("{key} needs input")); }
                    }
                    ui.label("Enter device and NIC addresses, selected input, and any output or configuration bytes. The fields below override the imported values.");
                });
                for issue in &p.unsupported { ui.colored_label(AMBER, issue); }
            }
            egui::Grid::new("network").num_columns(2).show(ui, |ui| {
                ui.label("Adapter IPv4");
                let device_changed = ui.text_edit_singleline(&mut self.device).changed();
                ui.end_row();
                ui.label("Local NIC IPv4");
                let local_changed = ui.text_edit_singleline(&mut self.local).changed();
                ui.end_row();
                if device_changed || local_changed {
                    self.reset_endpoint_status();
                    self.bench_confirmed = false;
                    self.writes_confirmed = false;
                }
                number(ui, "Local UDP port (0 = automatic)", &mut self.settings.local_udp_port);
            });
            let s = &mut self.settings;
            let write_settings = |s: &Settings| (
                s.input_only, s.output_instance, s.output_size, s.output_run_idle,
                s.config_instance, s.config_size, s.timeout_multiplier_code,
            );
            let before = write_settings(s);
            ui.separator();
            ui.checkbox(&mut s.input_only, "Input-only heartbeat profile");
            egui::Grid::new("assembly").num_columns(3).striped(true).show(ui, |ui| {
                ui.label(""); ui.strong("T->O input"); ui.strong("O->T output"); ui.end_row();
                ui.label("Connection point");
                ui.add(egui::DragValue::new(&mut s.input_instance));
                ui.add(egui::DragValue::new(&mut s.output_instance)); ui.end_row();
                ui.label("Assembly bytes");
                ui.add(egui::DragValue::new(&mut s.input_size));
                ui.add(egui::DragValue::new(&mut s.output_size)); ui.end_row();
                ui.label("Requested RPI (µs)");
                ui.add(egui::DragValue::new(&mut s.input_rpi_us).speed(10));
                ui.add(egui::DragValue::new(&mut s.output_rpi_us).speed(10)); ui.end_row();
                ui.label("32-bit Run/Idle");
                ui.checkbox(&mut s.input_run_idle, "");
                ui.checkbox(&mut s.output_run_idle, ""); ui.end_row();
                ui.label("CIP priority (0–3)");
                ui.add(egui::DragValue::new(&mut s.input_priority).range(0..=3));
                ui.add(egui::DragValue::new(&mut s.output_priority).range(0..=3)); ui.end_row();
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("CIP timeout code (0–7)");
                ui.add(
                    egui::DragValue::new(&mut s.timeout_multiplier_code)
                        .range(0..=model::MAX_TIMEOUT_MULTIPLIER_CODE)
                        .clamp_existing_to_range(false),
                )
                .on_hover_text("Sent in Forward_Open. The local receive watchdog uses the returned T->O interval.");
                if let Some(factor) = s.connection_timeout(None).factor {
                    ui.small(format!("×{factor}"));
                } else {
                    ui.colored_label(AMBER, "Enter a timeout code from 0 to 7");
                }
            });
            ui.horizontal(|ui| {
                let mut cfg = s.config_instance.is_some();
                if ui.checkbox(&mut cfg, "Configuration instance").changed() {
                    s.config_instance = cfg.then_some(1);
                }
                if let Some(instance) = &mut s.config_instance {
                    ui.add(egui::DragValue::new(instance));
                }
                ui.label("Bytes"); ui.add(egui::DragValue::new(&mut s.config_size));
            });
            ui.label("Configuration hex bytes (sent in Forward_Open)");
            if ui.add(egui::TextEdit::multiline(&mut s.config_hex).desired_rows(1).desired_width(f32::INFINITY).code_editor()).changed()
                || before != write_settings(s)
            {
                self.writes_confirmed = false;
            }
            title(ui, "02", "Assembly mappings");
            ui.collapsing("Primary input readback", |ui| {
                edit_selection(ui, &mut s.selection);
                if let Err(e) = s.selection.validate(s.input_size as usize) {
                    ui.colored_label(RED, e);
                }
            });
            ui.collapsing("Named inputs and CSV selection", |ui| {
                let mut all = s.log_fields.is_none();
                if ui.checkbox(&mut all, "Log all mapped inputs").changed() {
                    s.log_fields = if all { None } else if s.capture_fields.is_empty() {
                        Some(vec!["value".into()])
                    } else {
                        Some(s.capture_fields.iter().map(|field| field.name.clone()).collect())
                    };
                }
                let had_fields = !s.capture_fields.is_empty();
                if !had_fields {
                    ui.small("With no named inputs, CSV uses the primary readback.");
                    if let Some(names) = &mut s.log_fields {
                        let mut included = names.iter().any(|name| name == "value");
                        if ui.checkbox(&mut included, "Log primary value").changed() {
                            *names = if included { vec!["value".into()] } else { Vec::new() };
                        }
                    }
                }
                edit_fields(ui, "input", &mut s.capture_fields, s.log_fields.as_mut(), s.input_size);
                if had_fields == s.capture_fields.is_empty() {
                    if let Some(names) = &mut s.log_fields {
                        *names = if s.capture_fields.is_empty() { vec!["value".into()] }
                            else { s.capture_fields.iter().map(|field| field.name.clone()).collect() };
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("CSV rows");
                    ui.selectable_value(&mut s.capture_mode, CaptureMode::EveryPacket, "Every packet");
                    ui.selectable_value(&mut s.capture_mode, CaptureMode::OnChange, "On change");
                });
                match s.csv_fields() {
                    Ok(fields) => {
                        let names = fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>();
                        ui.small(format!("CSV columns · {}", names.join(", ")));
                    }
                    Err(e) => { ui.colored_label(AMBER, e); }
                }
            });
            if !s.input_only {
                ui.collapsing("Named output mappings", |ui| {
                    if edit_fields(ui, "output", &mut s.output_fields, None, s.output_size) {
                        self.output_entries.clear();
                    }
                });
            }
            if !online && !self.capturing {
                ui.collapsing("Preview input payload", |ui| self.show_input_preview(ui));
                ui.collapsing("Offline profile check", |ui| self.show_preflight(ui));
            }
        });
        if online || self.capturing {
            ui.small(
                "Setup and value selection are held while connected, reconnecting, or capturing.",
            );
        }
        ui.separator();
        ui.label("Staged output hex bytes");
        if ui
            .add_enabled(
                !self.settings.input_only,
                egui::TextEdit::multiline(&mut self.settings.output_hex)
                    .desired_rows(1)
                    .desired_width(f32::INFINITY)
                    .code_editor(),
            )
            .changed()
        {
            self.writes_confirmed = false;
        }
        self.typed_outputs(ui);
        if let Some(c) = &self.connection {
            if !self.settings.input_only {
                ui.checkbox(
                    &mut self.writes_confirmed,
                    "These staged output bytes are authorized",
                );
            }
            if ui
                .add_enabled(
                    !self.settings.input_only
                        && self.status.state == "Receiving"
                        && self.writes_confirmed,
                    egui::Button::new("Apply staged outputs"),
                )
                .clicked()
            {
                self.writes_confirmed = false;
                match self.settings.output() {
                    Ok(bytes) => {
                        *c.output.lock().unwrap() = bytes;
                        self.notice = Some("Output buffer applied to the cyclic producer".into());
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            let applied = c.output.lock().unwrap().clone();
            if !applied.is_empty() {
                ui.label(format!("Transmitting buffer · {}", model::hex(&applied)));
            }
            if ui.button("Disconnect").clicked() {
                self.disconnect();
            }
        } else if let Some((attempt, due)) = self.reconnect_pending() {
            ui.colored_label(
                AMBER,
                format!(
                    "Reconnect {attempt} of 3 in {:.1} s",
                    due.saturating_duration_since(Instant::now()).as_secs_f32()
                ),
            );
            if ui.button("Cancel reconnect").clicked() {
                self.disconnect();
                self.notice = Some("Automatic reconnect canceled".into());
            }
        } else {
            if !self.settings.input_only {
                ui.colored_label(
                    AMBER,
                    "Connecting requests output ownership and sends the staged output bytes.",
                );
            }
            if self.settings.config_size != 0 {
                ui.colored_label(AMBER, "Connecting applies the staged configuration bytes.");
            }
            if self.settings.output_run_idle {
                ui.colored_label(AMBER, "The scanner will assert Run in the O->T header.");
            }
            if !self
                .addresses()
                .is_ok_and(|(a, b)| a.is_loopback() && b.is_loopback())
            {
                ui.checkbox(
                    &mut self.bench_confirmed,
                    "I verified the adapter and isolated bench network",
                );
            }
            if !self.settings.input_only
                || self.settings.config_size != 0
                || self.settings.output_run_idle
            {
                ui.checkbox(
                    &mut self.writes_confirmed,
                    "Ownership and these output/configuration writes are authorized",
                );
            }
            if self.settings.input_only_without_writes() {
                ui.checkbox(
                    &mut self.preferences.auto_reconnect,
                    "Reconnect automatically",
                );
                if self.preferences.auto_reconnect && self.verified_identity.is_none() {
                    ui.small("Use Read identity to verify this device before connecting.");
                }
            }
            if let Some(pin) = self.verified_identity {
                ui.small(format!(
                    "Connect checks the verified identity · Serial {:08X}",
                    pin.serial
                ));
            }
            if self.identity_task.is_some() {
                ui.small("Reading identity…");
            }
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(self.identity_task.is_none(), egui::Button::new("Connect"))
                    .clicked()
                {
                    self.connect(reserved_ports);
                }
                ui.selectable_value(
                    &mut self.preferences.schedule,
                    Schedule::Deadline,
                    "Deadline",
                );
                #[cfg(windows)]
                ui.selectable_value(
                    &mut self.preferences.schedule,
                    Schedule::DeadlineAboveNormal,
                    "Deadline (above normal)",
                );
                ui.selectable_value(
                    &mut self.preferences.schedule,
                    Schedule::Tokio,
                    "Tokio baseline",
                );
            });
        }
        if let Some(e) = &self.error {
            ui.colored_label(RED, e);
        }
        if let Some(n) = &self.notice {
            ui.colored_label(CYAN, n);
        }
    }
    fn select_profile(&mut self, i: Option<usize>) {
        self.selected = i;
        self.writes_confirmed = false;
        self.output_entries.clear();
        if let Some(i) = i {
            let mut settings = self.profiles[i].settings.clone();
            settings.selection = self.settings.selection;
            settings.capture_fields = std::mem::take(&mut self.settings.capture_fields);
            settings.output_fields = std::mem::take(&mut self.settings.output_fields);
            settings.log_fields = self.settings.log_fields.take();
            settings.capture_mode = self.settings.capture_mode;
            settings.local_udp_port = self.settings.local_udp_port;
            settings.timeout_multiplier_code = self.settings.timeout_multiplier_code;
            self.settings = settings;
        }
    }
    fn show_preflight(&self, ui: &mut Ui) {
        ui.small("Checks the configuration locally. Verify the adapter identity and payload on the bench.");
        let settings = match self.current() {
            Ok(settings) => settings,
            Err(error) => {
                ui.colored_label(RED, error);
                return;
            }
        };
        let profile = self.selected.map(|i| &self.profiles[i]);
        let report = preflight::inspect(&settings, &self.preferences, profile);
        if report.connection_valid {
            ui.colored_label(CYAN, "Connection settings valid");
        }
        for error in &report.connection_errors {
            ui.colored_label(RED, error);
        }
        if report.capture_config_valid {
            ui.colored_label(CYAN, "Capture settings valid");
        }
        for error in &report.capture_errors {
            ui.colored_label(AMBER, error);
        }
        for (label, direction) in [
            ("T->O input", &report.input),
            ("O->T output", &report.output),
        ] {
            ui.label(format!(
                "{label} · Assembly {} · {} bytes · {} µs requested",
                direction.assembly, direction.payload_bytes, direction.requested_rpi_us
            ));
        }
        if let Some(us) = report.connection_timeout.requested_receive_watchdog_us {
            ui.small(format!(
                "Receive watchdog at requested input RPI · {:.3} ms",
                us as f64 / 1000.0
            ));
        }
        if report.requests_output_ownership {
            ui.colored_label(AMBER, "Connecting requests output ownership");
        }
        if report.asserts_run {
            ui.colored_label(AMBER, "Connecting asserts Run in the O->T header");
        }
        if report.configuration_bytes != 0 {
            ui.colored_label(
                AMBER,
                format!(
                    "Connecting sends {} configuration bytes",
                    report.configuration_bytes
                ),
            );
        }
        if !report.origins.is_empty() {
            egui::Grid::new("preflight-origins")
                .striped(true)
                .show(ui, |ui| {
                    for origin in report.origins {
                        ui.label(origin.field);
                        ui.monospace(origin.effective);
                        let response = ui.label(origin.state.label());
                        if let Some(value) = origin.eds {
                            response.on_hover_text(value);
                        }
                        ui.end_row();
                    }
                });
        }
    }
    fn show_input_preview(&mut self, ui: &mut Ui) {
        ui.small("Paste assembly bytes in hex, with spaces between bytes. Exclude sequence and Run/Idle headers.");
        ui.add(
            egui::TextEdit::multiline(&mut self.preview_hex)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .code_editor(),
        );
        if self.preview_hex.trim().is_empty() {
            return;
        }
        let preview = model::parse_hex(&self.preview_hex)
            .and_then(|bytes| self.settings.preview_input(&bytes));
        match preview {
            Ok(preview) => {
                egui::ScrollArea::both()
                    .id_salt("input-preview-scroll")
                    .max_height(200.0)
                    .show(ui, |ui| {
                        egui::Grid::new("input-preview")
                            .striped(true)
                            .show(ui, |ui| {
                                for heading in ["Field", "Offset", "Bytes", "Value", "CSV"] {
                                    ui.strong(heading);
                                }
                                ui.end_row();
                                for field in preview.fields {
                                    ui.label(field.name);
                                    ui.monospace(field.selection.offset.to_string());
                                    ui.monospace(field.raw_hex);
                                    ui.monospace(field.value);
                                    ui.label(if field.included_in_csv { "Yes" } else { "No" });
                                    ui.end_row();
                                }
                            });
                    });
            }
            Err(error) => {
                ui.colored_label(AMBER, error);
            }
        }
    }
    fn typed_outputs(&mut self, ui: &mut Ui) {
        if self.settings.input_only || self.settings.output_fields.is_empty() {
            return;
        }
        self.output_entries
            .resize_with(self.settings.output_fields.len(), String::new);
        ui.collapsing("Stage typed output values", |ui| {
            ui.small("Stage edits the hex buffer. Use Apply staged outputs to send it on an open connection.");
            let mut staged = self.settings.output();
            if staged.is_err() {
                ui.label("Enter a full hex buffer or initialize it explicitly.");
                if self.settings.output_size as usize <= model::MAX_ASSEMBLY_BYTES
                    && ui.button("Stage zero-filled buffer").clicked()
                {
                    let bytes = vec![0; self.settings.output_size as usize];
                    self.settings.output_hex = model::hex(&bytes);
                    self.writes_confirmed = false;
                    staged = Ok(bytes);
                }
            }
            let Ok(mut bytes) = staged else { return; };
            egui::Grid::new("typed-output").striped(true).show(ui, |ui| {
                ui.strong("Output"); ui.strong("Staged"); ui.strong("New value"); ui.label("");
                ui.end_row();
                for index in 0..self.settings.output_fields.len() {
                    let field = &self.settings.output_fields[index];
                    ui.label(&field.name);
                    ui.monospace(field.selection.decode(&bytes).map_or_else(|| "—".into(), |v| v.to_string()));
                    ui.push_id(index, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.output_entries[index]).desired_width(100.0));
                        if ui.button("Stage").clicked() {
                            let name = self.settings.output_fields[index].name.clone();
                            match self.settings.stage_output(index, &self.output_entries[index]) {
                                Ok(()) => {
                                    bytes = self.settings.output().expect("validated staged buffer");
                                    self.writes_confirmed = false;
                                    self.notice = Some(format!("Staged {name}"));
                                    self.error = None;
                                }
                                Err(error) => self.error = Some(format!("{name}: {error}")),
                            }
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }
    fn dashboard(&mut self, ui: &mut Ui) {
        title(ui, "03", "Live device");
        if let Some(id) = &self.status.identity {
            ui.label(RichText::new(&id.name).size(20.0).strong());
            ui.label(format!(
                "Vendor {}  ·  Type {}  ·  Product {}  ·  Revision {}.{}",
                id.vendor, id.device_type, id.code, id.major, id.minor
            ));
            ui.label(format!(
                "Serial {:08X}  ·  Device status 0x{:04X}",
                id.serial, id.status
            ));
        } else {
            ui.label("Identity unavailable");
        }
        if ui
            .add_enabled(
                !self.link_busy() && self.identity_task.is_none(),
                egui::Button::new("Read identity"),
            )
            .on_disabled_hover_text(
                "Available after disconnect and once the previous read finishes",
            )
            .clicked()
        {
            self.begin_identity_read();
        }
        if let Some(e) = &self.status.identity_error {
            ui.colored_label(AMBER, format!("Identity — {e}"));
        }
        if let Some(e) = &self.status.network_error {
            ui.colored_label(RED, format!("Transport — {e}"));
        }
        if let Some(e) = &self.status.cip_error {
            ui.colored_label(RED, format!("Connection Manager — {e}"));
        }
        if let Some(close) = &self.status.forward_close {
            ui.colored_label(
                if close.is_confirmed() { CYAN } else { AMBER },
                format!("Forward_Close {close}"),
            );
        }
        if let Some(active) = &self.active {
            egui::Grid::new("intervals")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Intervals in µs");
                    ui.strong("O->T");
                    ui.strong("T->O");
                    ui.end_row();
                    ui.label("Requested RPI");
                    ui.label(active.output_rpi_us.to_string());
                    ui.label(active.input_rpi_us.to_string());
                    ui.end_row();
                    ui.label("Forward_Open actual");
                    if let Some((ot, to)) = self.status.actual {
                        ui.label(ot.to_string());
                        ui.label(to.to_string());
                    } else {
                        ui.label("—");
                        ui.label("—");
                    }
                    ui.end_row();
                });
            if let Some(startup) = self.status.startup_timing(self.send_timing) {
                ui.small(format!(
                    "Forward_Open exchange {:.3} ms",
                    startup.forward_open_exchange_us as f64 / 1000.0
                ));
                if self.status.actual.is_some() {
                    let show = |us: Option<u64>| {
                        us.map_or_else(|| "—".into(), |us| format!("{:.3} ms", us as f64 / 1000.0))
                    };
                    ui.small(format!(
                        "After Forward_Open · First O->T send {} · First UDP {} · First valid input {}",
                        show(startup.first_send_after_open_us),
                        show(startup.first_udp_after_open_us),
                        show(startup.first_input_after_open_us)
                    ));
                }
            }
            if let Some(io) = self.status.cyclic_io {
                let input = io
                    .first_input_source
                    .map_or_else(|| "—".into(), |addr| addr.to_string());
                ui.small(format!(
                    "UDP local {} · O->T destination {} · First T->O source {input}",
                    io.local_udp, io.output_destination
                ));
                ui.small(format!(
                    "Connection IDs · O->T 0x{:08X} · T->O 0x{:08X}",
                    io.ot_connection_id, io.to_connection_id
                ));
            }
            let timeout = active.connection_timeout(self.status.actual.map(|(_, input)| input));
            if let (Some(factor), Some(us)) = (timeout.factor, timeout.receive_watchdog_us) {
                ui.small(format!(
                    "Receive watchdog {:.3} ms · CIP code {} · ×{factor}",
                    us as f64 / 1000.0,
                    timeout.multiplier_code
                ));
            }
        }
        let input_age = self.connection.as_ref().map_or_else(
            || self.status.last_input.map(|stamp| stamp.elapsed()),
            Connection::input_age,
        );
        ui.label(format!(
            "Last input age {}  ·  Timeouts {}",
            input_age.map_or_else(
                || "—".into(),
                |age| format!("{:.2} ms", age.as_secs_f64() * 1000.0)
            ),
            self.status.timeouts
        ));
        if let Some(timeout) = self.status.receive_timeout {
            let phase = if timeout.waiting_for_first_input {
                "First input timeout"
            } else {
                "Receive watchdog"
            };
            ui.colored_label(
                AMBER,
                format!(
                    "{phase} · Valid input silent for {:.3} ms · Limit {:.3} ms",
                    timeout.valid_input_silence_us as f64 / 1000.0,
                    timeout.limit_us as f64 / 1000.0,
                ),
            );
            if let Some(age) = timeout.last_udp_age_us {
                ui.small(format!(
                    "Last UDP datagram at timeout · {:.3} ms old",
                    age as f64 / 1000.0
                ));
            }
            if let Some(sequence) = timeout.last_sequence {
                ui.small(format!("Last accepted packet sequence · {sequence}"));
            }
            ui.small(format!(
                "O->T sends during silence {} · Receive check gap {:.3} ms (longest {:.3} ms)",
                timeout.ot_sends_during_silence,
                timeout.last_check_gap_us as f64 / 1000.0,
                timeout.longest_check_gap_us as f64 / 1000.0
            ));
        }
        ui.label(format!(
            "Accepted {}  ·  Gap estimates {}  ·  Duplicates {}  ·  Late {}  ·  Invalid {}",
            self.status.valid,
            self.status.gaps,
            self.status.duplicates,
            self.status.late,
            self.status.invalid
        ));
        ui.label(format!(
            "Sent {}  ·  Skipped deadline slots {}",
            self.tx, self.skipped
        ));
        if self.tx != 0 {
            ui.small(format!(
                "Longest UDP send call · {} µs · {} calls at or above O->T interval",
                self.send_timing.max_send_call_us, self.send_timing.send_calls_at_or_over_rpi
            ));
            if let Some(us) = self.send_timing.max_dispatch_lateness_us {
                ui.small(format!("Maximum producer dispatch lateness · {us} µs"));
            }
            if let Some(applied) = self.send_timing.above_normal_applied {
                ui.small(if applied {
                    "Producer thread priority · Above normal"
                } else {
                    "Producer thread priority · Above-normal request unavailable"
                });
            }
        }
        for (label, s) in [
            ("T->O receive", self.status.spacing),
            ("O->T send", self.tx_spacing),
        ] {
            if s.intervals == 0 {
                continue;
            }
            ui.label(format!(
                "{label} spacing in µs  ·  p50 {}  ·  p95 {}  ·  p99 {}  ·  longest {}",
                s.p50_us, s.p95_us, s.p99_us, s.max_us
            ));
            ui.small(format!(
                "Observed span {:.3} s  ·  {} intervals ≥2× negotiated RPI  ·  {} ≥4×",
                s.span_us as f64 / 1_000_000.0,
                s.intervals_at_or_over_2x_rpi,
                s.intervals_at_or_over_4x_rpi
            ));
            if let (Some(rate), Some(percent)) =
                (s.observed_packets_per_second, s.percent_of_negotiated_rate)
            {
                ui.small(format!(
                    "{rate:.1} packets/s  ·  {percent:.1}% of negotiated rate"
                ));
            }
        }
        ui.separator();
        title(ui, "04", "Input assembly");
        if let Some(v) = &self.status.value {
            ui.label(
                RichText::new(v.to_string())
                    .size(34.0)
                    .monospace()
                    .color(CYAN),
            );
        } else {
            ui.label("Waiting for an input sample");
        }
        if let Some(run) = self.status.input_run {
            ui.label(if run {
                "T->O header · Run"
            } else {
                "T->O header · Idle"
            });
        }
        if !self.status.input.is_empty() {
            if let Some(active) = &self.active {
                egui::ScrollArea::vertical()
                    .id_salt("mapped-input")
                    .max_height(200.0)
                    .show(ui, |ui| {
                        egui::Grid::new("input-readback")
                            .striped(true)
                            .show(ui, |ui| {
                                for field in &active.capture_fields {
                                    ui.label(&field.name);
                                    ui.monospace(
                                        field
                                            .selection
                                            .decode(&self.status.input)
                                            .map_or_else(|| "—".into(), |value| value.to_string()),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
            }
            egui::ScrollArea::vertical()
                .id_salt("raw-input")
                .max_height(116.0)
                .show(ui, |ui| {
                    for (row, bytes) in self.status.input.chunks(16).enumerate() {
                        ui.monospace(format!("{:04X}  {}", row * 16, model::hex(bytes)));
                    }
                });
        }
        ui.separator();
        title(ui, "05", "CSV capture");
        ui.add_enabled_ui(!self.capturing, |ui| {
            egui::Grid::new("capture").num_columns(2).show(ui, |ui| {
                ui.label("Output directory");
                ui.text_edit_singleline(&mut self.preferences.directory);
                ui.end_row();
                number(ui, "Buffer samples", &mut self.preferences.capacity);
                ui.label("Stop after seconds (0 = manual)");
                ui.add(
                    egui::DragValue::new(&mut self.preferences.gui_capture_seconds)
                        .range(0..=capture::MAX_CAPTURE_SECONDS)
                        .clamp_existing_to_range(false),
                );
                ui.end_row();
                number(
                    ui,
                    "Rotate after bytes (0 = off)",
                    &mut self.preferences.rotate_bytes,
                );
                number(
                    ui,
                    "Rotate after seconds (0 = off)",
                    &mut self.preferences.rotate_seconds,
                );
            });
        });
        if self.capturing {
            if ui.button("Stop capture and flush").clicked() {
                self.stop_capture();
            }
        } else if ui.button("Start CSV capture").clicked() {
            self.start_capture();
        }
        if let Some(capture) = &self.capture {
            let s = &capture.sink;
            if self.capturing && s.duration_seconds().is_some() {
                if let Some(deadline) = s.deadline() {
                    ui.small(format!(
                        "{:.2} s until capture stops",
                        deadline
                            .saturating_duration_since(Instant::now())
                            .as_secs_f64()
                    ));
                } else {
                    ui.small("Timer starts with the first input packet");
                }
            } else if self.capture_run.duration_reached {
                ui.small("Capture time limit reached");
            }
            let seen = s.seen.load(Relaxed);
            let unchanged = s.unchanged.load(Relaxed);
            let written = s.written.load(Relaxed);
            let overrun = s.overrun.load(Relaxed);
            ui.label(format!(
                "{}  ·  Seen {seen}  ·  Unchanged {unchanged}  ·  Written {written}",
                if self.capturing { "Armed" } else { "Stopped" }
            ));
            ui.colored_label(
                if overrun > 0 { RED } else { Color32::GRAY },
                format!("Local buffer overruns {overrun}"),
            );
            if let Some(e) = &*s.error.lock().unwrap() {
                ui.colored_label(
                    RED,
                    format!(
                        "Disk error — {e}  ·  Unconfirmed rows {}",
                        seen.saturating_sub(written + overrun + unchanged)
                    ),
                );
            }
            ui.small(s.prefix.display().to_string());
        }
        if self.capture.is_some()
            && ui
                .add_enabled(
                    !self.capturing && !self.link_busy(),
                    egui::Button::new("Save run report"),
                )
                .on_disabled_hover_text("Disconnect and stop capture before saving the report")
                .clicked()
        {
            match self.save_run_report() {
                Ok(path) => {
                    self.error = None;
                    self.notice = Some(format!("Saved run report to {}", path.display()));
                }
                Err(error) => {
                    self.notice = None;
                    self.error = Some(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary(name: &str) -> PathBuf {
        let mut id = [0; 8];
        getrandom::fill(&mut id).unwrap();
        std::env::temp_dir().join(format!("eipmaster-{name}-{:016x}", u64::from_le_bytes(id)))
    }

    #[test]
    fn migrates_single_device_and_restores_two_independent_setups() {
        let directory = temporary("session");
        let path = directory.join("last-session.toml");
        fs::create_dir(&directory).unwrap();
        let mut legacy = Device::new(1, None);
        legacy.profiles = eds::parse(include_str!("../samples/loopback.eds")).unwrap();
        legacy.settings.timeout_multiplier_code = 4;
        legacy.select_profile(Some(0));
        assert_eq!(legacy.settings.timeout_multiplier_code, 4);
        legacy.settings.input_rpi_us = 8_000;
        legacy.device = "in-progress address".into();
        legacy.local = "127.0.0.2".into();
        legacy.eds_path = "previous adapter.eds".into();
        legacy.toml_path = "saved profile.toml".into();
        legacy.preferences.rotate_seconds = 15;
        legacy.preferences.gui_capture_seconds = 10;
        legacy.preferences.schedule = Schedule::Tokio;
        let saved = legacy.snapshot();
        fs::write(&path, toml::to_string_pretty(&saved.setup).unwrap()).unwrap();

        let expected = {
            let mut app = App::new(Vec::new(), Some(path.clone()));
            assert_eq!(app.devices.len(), 1);
            assert_eq!(app.devices[0].snapshot(), saved);
            app.devices[0].name = "First adapter".into();
            app.devices[0].bench_confirmed = true;
            app.devices[0].writes_confirmed = true;
            app.devices[0]
                .output_entries
                .push("uncommitted output".into());
            app.add_device();
            let second = &mut app.devices[1];
            assert!(!second.bench_confirmed && !second.writes_confirmed);
            assert!(second.profiles.is_empty());
            second.name = "Second adapter".into();
            second.device = "127.0.0.3".into();
            second.preferences.directory = "second-capture".into();
            second.settings.input_rpi_us = 16_000;
            app.snapshot()
        };
        let restored = App::new(Vec::new(), Some(path.clone()));
        assert_eq!(restored.snapshot(), expected);
        assert_eq!(restored.selected, 1);
        for device in &restored.devices {
            assert!(!device.bench_confirmed && !device.writes_confirmed && !device.capturing);
            assert!(device.connection.is_none() && device.output_entries.is_empty());
        }
        drop(restored);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn refuses_a_reserved_fixed_udp_port_before_connecting() {
        let mut device = Device::new(1, None);
        device.settings.local_udp_port = 2222;
        device.connect(&[(Ipv4Addr::LOCALHOST, 2222)]);
        assert!(device.connection.is_none());
        assert!(
            device
                .error
                .as_deref()
                .unwrap()
                .contains("already reserved")
        );
    }

    #[test]
    fn automatic_reconnect_requires_an_identity_and_can_be_canceled() {
        let mut device = Device::new(1, None);
        device.preferences.auto_reconnect = true;
        device.connect(&[]);
        assert!(device.connection.is_none());
        assert!(device.error.as_deref().unwrap().contains("Read identity"));

        let pin = IdentityPin {
            key: (0, 12, 1),
            serial: 1,
        };
        let mut reconnect = Reconnect::new(pin);
        assert!(reconnect.schedule(Instant::now()));
        device.reconnect = Some(reconnect);
        assert!(device.link_busy());
        device.disconnect();
        device.poll();
        assert!(!device.link_busy());

        device.verified_identity = Some(pin);
        device.reset_endpoint_status();
        assert!(device.verified_identity.is_none());
    }

    #[test]
    fn disk_failure_releases_only_the_failed_capture_slot() {
        let directory = temporary("capture-isolation");
        let mut healthy = Device::new(1, None);
        healthy.preferences.directory = directory.join("healthy").display().to_string();
        healthy.start_capture();
        let mut failing = Device::new(2, None);
        failing.preferences.directory = directory.join("failing").display().to_string();
        failing.preferences.rotate_bytes = 1;
        failing.start_capture();
        let failing_sink = failing.capture.as_ref().unwrap().sink.clone();
        let sentinel = PathBuf::from(format!("{}-000002.csv", failing_sink.prefix.display()));
        fs::write(&sentinel, "existing file").unwrap();
        for sequence in 1..=2 {
            failing_sink.record(Instant::now(), 1, sequence, sequence as u16, &[0; 4]);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while failing.capturing && Instant::now() < deadline {
            failing.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!failing.capturing && failing.slot.read().unwrap().is_none());
        assert!(failing_sink.error.lock().unwrap().is_some());
        assert!(healthy.capturing && healthy.slot.read().unwrap().is_some());
        let healthy_sink = healthy.capture.as_ref().unwrap().sink.clone();
        healthy_sink.record(Instant::now(), 1, 1, 1, &[0; 4]);
        healthy.stop_capture();
        assert_eq!(healthy_sink.written.load(Relaxed), 1);
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "existing file");
        let report_path = failing.save_run_report().unwrap();
        let report: toml::Table =
            toml::from_str(&fs::read_to_string(report_path).unwrap()).unwrap();
        assert!(report.get("connection").is_none());
        assert!(report["attempts"].as_array().unwrap().is_empty());
        assert!(report["capture"]["disk_error"].as_str().is_some());
        assert_eq!(report["capture"]["unconfirmed_rows"].as_integer(), Some(1));
        assert_eq!(report["capture"]["buffer_overruns"].as_integer(), Some(0));
        assert!(report["capture"].get("field_summaries").is_none());
        for name in ["healthy", "failing"] {
            for file in fs::read_dir(directory.join(name)).unwrap() {
                fs::remove_file(file.unwrap().path()).unwrap();
            }
            fs::remove_dir(directory.join(name)).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn run_report_keeps_only_connections_that_belonged_to_the_capture() {
        let directory = temporary("run-report");
        let mut device = Device::new(1, None);
        device.preferences.directory = directory.display().to_string();
        device.settings.capture_mode = CaptureMode::OnChange;
        device.preferences.gui_capture_seconds = 60;
        device.start_capture();
        assert!(device.save_run_report().is_err());
        let sink = device.capture.as_ref().unwrap().sink.clone();
        device.capture_run.track(1);
        sink.record(Instant::now(), 1, 1, 1, &[0; 4]);
        sink.record(Instant::now(), 1, 2, 2, &[0; 4]);
        device.generation = 1;
        device.active = Some(Settings {
            device_ip: Ipv4Addr::new(127, 0, 0, 2),
            output_rpi_us: 1000,
            input_rpi_us: 1000,
            ..Settings::default()
        });
        device.status = Status {
            state: "Closed",
            actual: Some((1000, 1000)),
            valid: 2,
            ..Status::default()
        };
        device.record_capture_connection();

        device.capture_run.track(2);
        sink.record(Instant::now(), 2, 1, 1, &[0; 4]);
        device.stop_capture();
        device.generation = 2;
        device.status = Status {
            state: "Timed out",
            actual: Some((1250, 1500)),
            valid: 1,
            timeouts: 1,
            receive_timeout: Some(scanner::ReceiveTimeout {
                waiting_for_first_input: false,
                limit_us: 48_000,
                valid_input_silence_us: 48_100,
                last_udp_age_us: Some(48_100),
                last_sequence: Some(1),
                ot_sends_during_silence: 38,
                last_check_gap_us: 2_100,
                longest_check_gap_us: 2_300,
            }),
            ..Status::default()
        };
        device.record_capture_connection();

        device.generation = 3;
        device.preferences.schedule = Schedule::Tokio;
        device.status.state = "Closed";
        device.record_capture_connection();
        device.reset_endpoint_status();

        let path = device.save_run_report().unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!(
                "{}-report.toml",
                sink.prefix.file_name().unwrap().to_string_lossy()
            )
        );
        let text = fs::read_to_string(&path).unwrap();
        let report: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(report["mode"].as_str(), Some("gui"));
        let capture = &report["capture"];
        assert_eq!(capture["mode"].as_str(), Some("on_change"));
        assert_eq!(capture["duration_seconds"].as_integer(), Some(60));
        assert_eq!(capture["duration_reached"].as_bool(), Some(false));
        assert_eq!(capture["started_utc"].as_str(), Some(sink.start_utc()));
        assert_eq!(capture["csv_parts"].as_integer(), Some(1));
        assert_eq!(capture["first_armed_generation"].as_integer(), Some(1));
        assert_eq!(capture["last_armed_generation"].as_integer(), Some(2));
        assert_eq!(capture["seen"].as_integer(), Some(3));
        assert_eq!(capture["written"].as_integer(), Some(2));
        assert_eq!(capture["unchanged"].as_integer(), Some(1));
        let summary = &capture["field_summaries"].as_array().unwrap()[0];
        assert_eq!(summary["name"].as_str(), Some("value"));
        assert_eq!(summary["first_value"].as_str(), Some("0"));
        assert_eq!(summary["last_value"].as_str(), Some("0"));
        assert_eq!(summary["changes_within_generation"].as_integer(), Some(0));
        let connection = &report["connection"];
        let attempts = report["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["generation"].as_integer(), Some(1));
        assert_eq!(attempts[0]["state"].as_str(), Some("Closed"));
        assert_eq!(attempts[0]["accepted"].as_integer(), Some(2));
        assert_eq!(&attempts[1], connection);
        assert_eq!(connection["generation"].as_integer(), Some(2));
        assert_eq!(connection["adapter_ip"].as_str(), Some("127.0.0.2"));
        assert_eq!(connection["state"].as_str(), Some("Timed out"));
        assert_eq!(connection["schedule"].as_str(), Some("deadline"));
        assert_eq!(connection["requested_to_us"].as_integer(), Some(1000));
        assert_eq!(connection["actual_to_us"].as_integer(), Some(1500));
        assert_eq!(
            connection["receive_timeout"]["limit_us"].as_integer(),
            Some(48_000)
        );
        assert_eq!(
            connection["receive_timeout"]["ot_sends_during_silence"].as_integer(),
            Some(38)
        );
        assert!(device.save_run_report().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        remove_capture_directory(directory);
    }

    fn remove_capture_directory(directory: PathBuf) {
        for file in fs::read_dir(&directory).unwrap() {
            fs::remove_file(file.unwrap().path()).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }

    #[track_caller]
    fn poll_until(device: &mut Device, ready: impl Fn(&Device) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            device.poll();
            if ready(device) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "device state {}",
                device.state_label()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires tools/loopback_adapter.py --mode normal on localhost"]
    fn timed_gui_capture_finishes_after_first_input_even_when_polling_is_delayed() {
        let directory = temporary("gui-timed-capture");
        let mut device = Device::new(1, None);
        device.preferences.directory = directory.display().to_string();
        device.preferences.gui_capture_seconds = 1;
        device.preferences.rotate_bytes = 2048;
        device.settings.input_rpi_us = 8000;
        device.settings.output_rpi_us = 8000;
        device.start_capture();
        let sink = device.capture.as_ref().unwrap().sink.clone();
        std::thread::sleep(Duration::from_millis(1050));
        device.poll();
        assert!(device.capturing && sink.deadline().is_none());

        device.connect(&[]);
        poll_until(&mut device, |_| sink.deadline().is_some());
        let remaining = sink
            .deadline()
            .unwrap()
            .saturating_duration_since(Instant::now());
        std::thread::sleep(remaining + Duration::from_millis(200));
        device.poll();
        assert!(!device.capturing && device.capture_run.duration_reached);
        assert!(device.link_busy());
        let written = sink.written.load(Relaxed);
        assert!(written > 100 && sink.part_count() > 1);
        assert_eq!(sink.seen.load(Relaxed), written);
        poll_until(&mut device, |device| device.status.valid > written);
        device.disconnect();
        poll_until(&mut device, |device| !device.link_busy());

        let path = device.save_run_report().unwrap();
        let report: toml::Table = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let capture = &report["capture"];
        assert_eq!(capture["duration_seconds"].as_integer(), Some(1));
        assert_eq!(capture["duration_reached"].as_bool(), Some(true));
        assert_eq!(capture["written"].as_integer(), Some(written as i64));
        let after_deadline = capture["after_capture_deadline"].as_integer().unwrap();
        assert!(after_deadline > 0);
        let link = &report["connection"];
        assert_eq!(link["forward_close"]["outcome"].as_str(), Some("confirmed"));
        let accepted = link["accepted"].as_integer().unwrap();
        assert!(accepted >= written as i64 + after_deadline);
        eprintln!("Timed GUI capture saved {written} rows of {accepted} connection packets");
        remove_capture_directory(directory);
    }

    #[test]
    fn pending_identity_blocks_connect_and_failed_refresh_keeps_the_pin() {
        let mut device = Device::new(1, None);
        let pin = IdentityPin {
            key: (0, 12, 1),
            serial: 0x12345678,
        };
        device.verified_identity = Some(pin);
        let (tx, rx) = mpsc::channel();
        device.identity_task = Some(rx);
        device.connect(&[]);
        assert!(device.connection.is_none());
        assert_eq!(device.generation, 0);
        assert_eq!(
            device.error.as_deref(),
            Some("Wait for the identity read to finish")
        );

        tx.send(Err("Fixture identity read failed".into())).unwrap();
        device.poll();
        assert!(device.identity_task.is_none());
        assert_eq!(device.verified_identity, Some(pin));
        assert_eq!(
            device.status.identity_error.as_deref(),
            Some("Fixture identity read failed")
        );
        device.reset_endpoint_status();
        assert!(device.verified_identity.is_none());
    }

    fn read_loopback_owner_identity() -> Device {
        let mut device = Device::new(1, None);
        let settings = &mut device.settings;
        settings.input_only = false;
        settings.output_instance = 100;
        settings.output_size = 2;
        settings.output_hex = "12 34".into();
        settings.input_run_idle = true;
        settings.output_run_idle = true;
        settings.config_size = 3;
        settings.config_hex = "AA BB CC".into();
        settings.input_rpi_us = 8000;
        settings.output_rpi_us = 8000;
        device.begin_identity_read();
        device.bench_confirmed = true;
        device.writes_confirmed = true;
        poll_until(&mut device, |device| device.identity_task.is_none());
        assert!(!device.bench_confirmed && !device.writes_confirmed);
        assert_eq!(
            device.verified_identity,
            Some(IdentityPin {
                key: (0, 12, 1),
                serial: 0x12345678,
            })
        );
        device
    }

    #[test]
    #[ignore = "requires tools/loopback_adapter.py --mode qualify-replacement on localhost"]
    fn manual_pinned_connection_blocks_a_replacement_before_writes() {
        let mut device = read_loopback_owner_identity();
        device.writes_confirmed = true;
        device.connect(&[]);
        poll_until(&mut device, |device| !device.link_busy());
        assert_eq!(device.status.state, "Error");
        assert_eq!(device.status.identity.as_ref().unwrap().serial, 0x12345679);
        let error = device.status.identity_error.as_deref().unwrap();
        assert!(error.contains("Identity mismatch") && error.contains("12345678"));
        assert!(device.status.actual.is_none());
        assert_eq!(device.tx, 0);
    }

    #[test]
    #[ignore = "requires tools/loopback_adapter.py --mode identity-then-owner on localhost"]
    fn matching_pin_allows_separately_authorized_owner_connection() {
        let mut device = read_loopback_owner_identity();
        device.connect(&[]);
        assert!(device.connection.is_none());
        assert!(
            device
                .error
                .as_deref()
                .unwrap()
                .contains("output permission")
        );

        device.writes_confirmed = true;
        device.connect(&[]);
        poll_until(&mut device, |device| device.status.valid >= 25);
        assert!(device.reconnect.is_none());
        assert!(!device.writes_confirmed);
        device.disconnect();
        poll_until(&mut device, |device| !device.link_busy());
        assert_eq!(device.status.state, "Closed");
        assert!(device.status.forward_close.as_ref().unwrap().is_confirmed());
    }

    #[test]
    #[ignore = "requires tools/loopback_adapter.py --mode different-api on localhost"]
    fn run_report_uses_the_final_loopback_connection() {
        let directory = temporary("loopback-run-report");
        let mut device = Device::new(1, None);
        device.preferences.directory = directory.display().to_string();
        device.settings.timeout_multiplier_code = 4;
        device.start_capture();
        device.connect(&[]);
        poll_until(&mut device, |device| device.status.valid >= 50);
        device.disconnect();
        poll_until(&mut device, |device| !device.link_busy());
        device.stop_capture();
        let path = device.save_run_report().unwrap();
        let report: toml::Table = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            report["capture"]["first_armed_generation"].as_integer(),
            Some(1)
        );
        assert_eq!(
            report["capture"]["last_armed_generation"].as_integer(),
            Some(1)
        );
        let link = &report["connection"];
        assert_eq!(
            report["attempts"].as_array().unwrap().as_slice(),
            std::slice::from_ref(link)
        );
        assert_eq!(link["state"].as_str(), Some("Closed"));
        assert_eq!(link["forward_close"]["outcome"].as_str(), Some("confirmed"));
        assert_eq!(link["requested_ot_us"].as_integer(), Some(1000));
        assert_eq!(link["requested_to_us"].as_integer(), Some(1000));
        assert_eq!(link["actual_ot_us"].as_integer(), Some(1250));
        assert_eq!(link["actual_to_us"].as_integer(), Some(1500));
        let io = &link["cyclic_io"];
        let local: std::net::SocketAddr = io["local_udp"].as_str().unwrap().parse().unwrap();
        assert!(local.ip().is_loopback() && local.port() != 0);
        assert_eq!(io["first_input_source"], io["output_destination"]);
        assert_eq!(io["ot_connection_id"].as_integer(), Some(0x1234ABCD));
        assert!(io["to_connection_id"].as_integer().unwrap() > 0);
        let startup = &link["startup_timing"];
        assert!(startup["forward_open_exchange_us"].is_integer());
        assert!(startup["first_send_after_open_us"].is_integer());
        assert_eq!(
            startup["first_udp_after_open_us"],
            startup["first_input_after_open_us"]
        );
        let timeout = &link["connection_timeout"];
        assert_eq!(timeout["multiplier_code"].as_integer(), Some(4));
        assert_eq!(
            timeout["requested_receive_watchdog_us"].as_integer(),
            Some(64_000)
        );
        assert_eq!(timeout["receive_watchdog_us"].as_integer(), Some(96_000));
        assert_eq!(
            link["receive_spacing"]["intervals"].as_integer(),
            Some(device.status.valid as i64 - 1)
        );
        assert_eq!(
            report["capture"]["written"].as_integer(),
            link["accepted"].as_integer()
        );
        assert_eq!(report["capture"]["unconfirmed_rows"].as_integer(), Some(0));
        remove_capture_directory(directory);
    }

    #[test]
    #[ignore = "requires tools/loopback_adapter.py --mode recover on localhost"]
    fn run_report_keeps_loopback_reconnect_attempts() {
        let directory = temporary("reconnect-run-report");
        let mut device = Device::new(1, None);
        device.preferences.directory = directory.display().to_string();
        device.preferences.rotate_bytes = 2048;
        device.preferences.auto_reconnect = true;
        device.settings.input_rpi_us = 8000;
        device.settings.output_rpi_us = 8000;
        device.verified_identity = Some(IdentityPin {
            key: (0, 12, 1),
            serial: 0x12345678,
        });
        device.start_capture();
        device.connect(&[]);
        poll_until(&mut device, |device| {
            device.generation == 2 && device.status.valid >= 50
        });
        device.disconnect();
        poll_until(&mut device, |device| !device.link_busy());
        device.stop_capture();

        let path = device.save_run_report().unwrap();
        let report: toml::Table = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let [first, restored] = report["attempts"].as_array().unwrap().as_slice() else {
            panic!("expected two connection attempts");
        };
        assert_eq!(first["generation"].as_integer(), Some(1));
        assert_eq!(first["state"].as_str(), Some("Timed out"));
        assert_eq!(
            first["receive_timeout"]["limit_us"].as_integer(),
            Some(256_000)
        );
        let timeout = &first["receive_timeout"];
        assert!(timeout["ot_sends_during_silence"].as_integer().unwrap() > 0);
        let last_gap = timeout["last_check_gap_us"].as_integer().unwrap();
        let longest_gap = timeout["longest_check_gap_us"].as_integer().unwrap();
        assert!(0 < last_gap && last_gap <= longest_gap);
        assert_eq!(restored["generation"].as_integer(), Some(2));
        assert_eq!(restored["state"].as_str(), Some("Closed"));
        assert!(restored.get("receive_timeout").is_none());
        assert_eq!(&report["connection"], restored);
        for attempt in [first, restored] {
            assert_eq!(attempt["actual_ot_us"].as_integer(), Some(8000));
            assert_eq!(attempt["actual_to_us"].as_integer(), Some(8000));
            assert_eq!(attempt["identity"]["serial"].as_integer(), Some(0x12345678));
            assert_eq!(
                attempt["forward_close"]["outcome"].as_str(),
                Some("confirmed")
            );
        }
        let capture = &report["capture"];
        let accepted =
            first["accepted"].as_integer().unwrap() + restored["accepted"].as_integer().unwrap();
        assert_eq!(capture["written"].as_integer(), Some(accepted));
        assert_eq!(capture["buffer_overruns"].as_integer(), Some(0));
        assert!(capture["csv_parts"].as_integer().unwrap() > 1);
        eprintln!("GUI report saved {accepted} rows across two attempts");
        remove_capture_directory(directory);
    }

    #[test]
    fn named_profile_loads_preferences_and_requires_confirmation_to_replace() {
        let directory = temporary("profiles");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("profile.toml");
        let mut app = Device::new(1, None);
        app.toml_path = path.display().to_string();
        app.device = "127.0.0.2".into();
        app.preferences.capacity = 200;
        app.save_profile(None);
        assert!(app.error.is_none());
        let original = fs::read(&path).unwrap();
        app.preferences.capacity = 800;
        app.preferences.rotate_bytes = 4_096;
        app.save_profile(None);
        assert_eq!(app.replace_profile.as_deref(), Some(path.as_path()));
        assert_eq!(fs::read(&path).unwrap(), original);
        let confirmed = app.replace_profile.take();
        app.save_profile(confirmed);
        assert!(app.error.is_none());
        app.device = "127.0.0.3".into();
        app.preferences = Preferences::default();
        app.load_profile();
        assert!(app.error.is_none());
        assert_eq!(app.device, "127.0.0.2");
        assert_eq!(app.preferences.capacity, 800);
        assert_eq!(app.preferences.rotate_bytes, 4_096);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
