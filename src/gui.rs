use crate::{
    capture::{self, Capture},
    eds::{self, Profile},
    model::{self, Kind, Settings},
    scanner::{self, Connection, Identity, Schedule, Spacing, Status},
};
use eframe::egui::{self, Color32, RichText, Ui};
use std::{
    fs,
    net::Ipv4Addr,
    path::PathBuf,
    sync::{atomic::Ordering::Relaxed, mpsc},
    time::{Duration, Instant},
};

const CYAN: Color32 = Color32::from_rgb(99, 226, 213);
const AMBER: Color32 = Color32::from_rgb(247, 191, 101);
const RED: Color32 = Color32::from_rgb(255, 119, 119);

pub fn run() -> eframe::Result {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let bench = if matches!(args.len(), 3 | 4) && args[0] == "--loopback-benchmark" {
        let schedule = if args[1] == "tokio" {
            Schedule::Tokio
        } else {
            Schedule::Deadline
        };
        Some(Benchmark {
            schedule,
            directory: PathBuf::from(&args[2]),
            started: Instant::now(),
            stopping: false,
            owner: args.len() == 3 && args[1] == "owner",
            profile: args.get(3).map(PathBuf::from),
            reconnect: args[1] == "reconnect",
            transitioned: false,
        })
    } else {
        None
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
            Ok(Box::new(App::new(bench)))
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
}
struct App {
    settings: Settings,
    device: String,
    local: String,
    eds_path: String,
    toml_path: String,
    profiles: Vec<Profile>,
    selected: Option<usize>,
    bench_confirmed: bool,
    writes_confirmed: bool,
    connection: Option<Connection>,
    active: Option<Settings>,
    status: Status,
    generation: u64,
    schedule: Schedule,
    tx: u64,
    skipped: u64,
    tx_spacing: Spacing,
    capture: Option<Capture>,
    slot: capture::Slot,
    capturing: bool,
    directory: String,
    capacity: usize,
    rotate_bytes: u64,
    rotate_seconds: u64,
    notice: Option<String>,
    error: Option<String>,
    identity_task: Option<mpsc::Receiver<Result<Identity, String>>>,
    bench: Option<Benchmark>,
}
impl App {
    fn new(bench: Option<Benchmark>) -> Self {
        let mut app = Self {
            settings: Settings::default(),
            device: "127.0.0.1".into(),
            local: "127.0.0.1".into(),
            eds_path: "samples/loopback.eds".into(),
            toml_path: "samples/loopback.toml".into(),
            profiles: vec![],
            selected: None,
            bench_confirmed: false,
            writes_confirmed: false,
            connection: None,
            active: None,
            status: Status::default(),
            generation: 0,
            schedule: Schedule::Deadline,
            tx: 0,
            skipped: 0,
            tx_spacing: Spacing::default(),
            capture: None,
            slot: Default::default(),
            capturing: false,
            directory: "captures".into(),
            capacity: 32_768,
            rotate_bytes: 0,
            rotate_seconds: 0,
            notice: None,
            error: None,
            identity_task: None,
            bench,
        };
        if let Some(bench) = &app.bench {
            app.directory = bench.directory.to_string_lossy().into_owned();
            app.schedule = bench.schedule;
            app.rotate_seconds = 3;
            if let Some(profile) = &bench.profile {
                let result = fs::read_to_string(profile)
                    .map_err(|error| error.to_string())
                    .and_then(|text| toml::from_str(&text).map_err(|error| error.to_string()));
                match result {
                    Ok(settings) => app.settings = settings,
                    Err(error) => {
                        app.error = Some(format!("Could not load loopback settings: {error}"));
                        return app;
                    }
                }
            }
            if bench.owner {
                app.settings.input_only = false;
                app.settings.output_instance = 100;
                app.settings.output_size = 2;
                app.settings.output_run_idle = true;
                app.settings.input_run_idle = true;
                app.settings.output_hex = "12 34".into();
                app.settings.config_size = 3;
                app.settings.config_hex = "AA BB CC".into();
                app.writes_confirmed = true;
            }
            app.start_capture();
            app.connect();
        }
        app
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
    fn connect(&mut self) {
        self.error = None;
        self.notice = None;
        let result = (|| {
            let s = self.current()?;
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
            let next = self.generation + 1;
            let connection = Connection::start(
                s.clone(),
                self.slot.clone(),
                next,
                self.schedule,
                self.writes_confirmed,
            )?;
            self.generation = next;
            self.active = Some(s);
            self.status = Status::default();
            self.tx = 0;
            self.skipped = 0;
            self.tx_spacing = Spacing::default();
            self.identity_task = None;
            self.connection = Some(connection);
            Ok(())
        })();
        self.bench_confirmed = false;
        self.writes_confirmed = false;
        if let Err(e) = result {
            self.error = Some(e);
        }
    }
    fn start_capture(&mut self) {
        let settings = self
            .active
            .as_ref()
            .filter(|_| self.connection.is_some())
            .unwrap_or(&self.settings);
        match Capture::start(
            &self.slot,
            capture::Options {
                directory: PathBuf::from(&self.directory),
                capacity: self.capacity,
                rotate_bytes: self.rotate_bytes,
                rotate_seconds: self.rotate_seconds,
            },
            settings,
        ) {
            Ok(capture) => {
                self.capture = Some(capture);
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
            self.tx = c.tx.load(Relaxed);
            self.skipped = c.skipped.load(Relaxed);
            self.tx_spacing = *c.tx_spacing.lock().unwrap();
            if c.finished() {
                let c = self.connection.take().unwrap();
                self.status = c.status.lock().unwrap().clone();
                c.join();
            }
        }
        if let Some(task) = &self.identity_task {
            match task.try_recv() {
                Ok(result) => {
                    let update = |status: &mut Status| match &result {
                        Ok(identity) => {
                            status.identity = Some(identity.clone());
                            status.identity_error = None;
                        }
                        Err(error) => status.identity_error = Some(error.clone()),
                    };
                    update(&mut self.status);
                    if let Some(c) = &self.connection {
                        update(&mut c.status.lock().unwrap());
                    }
                    self.identity_task = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.identity_task = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn benchmark(&mut self, ctx: &egui::Context) {
        let Some(bench) = &mut self.bench else {
            return;
        };
        if bench.started.elapsed() >= Duration::from_secs(4) && !bench.transitioned {
            if bench.owner && self.status.state == "Receiving" {
                if let Some(c) = &self.connection {
                    *c.output.lock().unwrap() = vec![0x56, 0x78];
                }
                bench.transitioned = true;
            } else if bench.reconnect && self.generation == 1 {
                if let Some(c) = &self.connection {
                    c.stop();
                    return;
                }
                bench.transitioned = true;
                self.connect();
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
        let cip_error = self.status.cip_error.as_deref().unwrap_or_default();
        let network_error = self.status.network_error.as_deref().unwrap_or_default();
        let (ot, to) = self.status.actual.unwrap_or_default();
        let (requested_ot, requested_to) = self.active.as_ref().map_or(
            (self.settings.output_rpi_us, self.settings.input_rpi_us),
            |s| (s.output_rpi_us, s.input_rpi_us),
        );
        let setup_error = self.error.as_deref().unwrap_or_default();
        let (seen, written, overrun, disk_error) = self
            .capture
            .as_ref()
            .map(|capture| {
                let s = &capture.sink;
                (
                    s.seen.load(Relaxed),
                    s.written.load(Relaxed),
                    s.overrun.load(Relaxed),
                    s.error.lock().unwrap().clone(),
                )
            })
            .unwrap_or_default();
        let text = format!(
            "state = {:?}\nschedule = {:?}\nrequested_ot_us = {requested_ot}\nrequested_to_us = {requested_to}\nactual_ot_us = {ot}\nactual_to_us = {to}\nvalid = {}\ngaps = {}\nduplicates = {}\nlate = {}\ninvalid = {}\ntimeouts = {}\ntx = {}\nskipped_deadlines = {}\nseen = {seen}\nwritten = {written}\noverruns = {overrun}\ndisk_error = {}\ngenerations = {}\ncip_error = {cip_error:?}\nnetwork_error = {network_error:?}\nsetup_error = {setup_error:?}\n\n[spacing]\n{spacing}\n[tx_spacing]\n{tx_spacing}",
            self.status.state,
            format!("{:?}", self.schedule),
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
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if let Some(c) = self.connection.take() {
            c.stop();
            c.join();
        }
        self.stop_capture();
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        self.benchmark(ctx);
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
                    let state = if self.status.state.is_empty() {
                        "Offline"
                    } else {
                        self.status.state
                    };
                    ui.label(RichText::new(state).strong().color(
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
                egui::ScrollArea::vertical().show(ui, |ui| self.setup(ui));
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.dashboard(ui));
        });
        if online || self.capturing || self.bench.is_some() || self.identity_task.is_some() {
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
impl App {
    fn setup(&mut self, ui: &mut Ui) {
        let online = self.connection.is_some();
        title(ui, "01", "Device setup");
        ui.add_enabled_ui(!online && !self.capturing, |ui| {
            ui.horizontal(|ui| {
                ui.label("Settings TOML");
                ui.text_edit_singleline(&mut self.toml_path);
                if ui.button("Load").clicked() {
                    let result = fs::read_to_string(&self.toml_path).map_err(|e| e.to_string())
                        .and_then(|text| toml::from_str::<Settings>(&text).map_err(|e| e.to_string()));
                    match result {
                        Ok(settings) => {
                            self.device = settings.device_ip.to_string();
                            self.local = settings.local_ip.to_string();
                            self.settings = settings;
                            self.selected = None;
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                if ui.button("Save").clicked() {
                    let result = self.current().and_then(|s| {
                        s.validate()?;
                        toml::to_string_pretty(&s).map_err(|e| e.to_string())
                    }).and_then(|s| {
                        use std::io::Write;
                        fs::OpenOptions::new().create_new(true).write(true).open(&self.toml_path)
                            .and_then(|mut f| f.write_all(s.as_bytes())).map_err(|e| e.to_string())
                    });
                    match result {
                        Ok(()) => { self.error = None; self.notice = Some("Saved settings".into()); }
                        Err(e) => self.error = Some(format!("Save needs a new filename — {e}")),
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("EDS file");
                ui.text_edit_singleline(&mut self.eds_path);
                if ui.button("Import").clicked() {
                    match fs::read(&self.eds_path).map_err(|e| e.to_string())
                        .and_then(|bytes| eds::parse(&String::from_utf8_lossy(&bytes))) {
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
                    for (key, value) in &p.fields { ui.label(format!("{key} = {value}")); }
                    for key in ["O->T RPI", "T->O RPI", "T->O size"] {
                        if !p.fields.contains_key(key) { ui.colored_label(AMBER, format!("{key} needs input")); }
                    }
                    ui.label("Enter device and NIC addresses, selected input, and any output or configuration bytes. The fields below override the imported values.");
                });
                for issue in &p.unsupported { ui.colored_label(AMBER, issue); }
            }
            egui::Grid::new("network").num_columns(2).show(ui, |ui| {
                ui.label("Adapter IPv4"); ui.text_edit_singleline(&mut self.device); ui.end_row();
                ui.label("Local NIC IPv4"); ui.text_edit_singleline(&mut self.local); ui.end_row();
                number(ui, "Local UDP port (0 = automatic)", &mut self.settings.local_udp_port);
            });
            let s = &mut self.settings;
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
            ui.add(egui::TextEdit::multiline(&mut s.config_hex).desired_rows(1).desired_width(f32::INFINITY).code_editor());
            title(ui, "02", if s.capture_fields.is_empty() { "Value to capture" } else { "Primary readback" });
            let selection = &mut s.selection;
            ui.horizontal_wrapped(|ui| {
                ui.label("Byte offset");
                ui.add(egui::DragValue::new(&mut selection.offset));
                let mut bit = selection.bit.is_some();
                if ui.checkbox(&mut bit, "Single bit").changed() { selection.bit = bit.then_some(0); }
                if let Some(bit) = &mut selection.bit { ui.add(egui::DragValue::new(bit).range(0..=7)); }
                else {
                    egui::ComboBox::from_id_salt("value-kind").selected_text(format!("{:?}", selection.kind)).show_ui(ui, |ui| {
                        for kind in Kind::ALL { ui.selectable_value(&mut selection.kind, kind, format!("{kind:?}")); }
                    });
                    ui.checkbox(&mut selection.big_endian, "Big endian");
                }
            });
            if let Err(e) = selection.validate(s.input_size as usize) { ui.colored_label(RED, e); }
            if !s.capture_fields.is_empty() {
                let names = s.capture_fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>();
                ui.small(format!("CSV fields from settings: {}", names.join(", ")));
            }
        });
        if online || self.capturing {
            ui.small("Setup and value selection are held while connected or capturing.");
        }
        ui.separator();
        ui.label("Staged output hex bytes");
        ui.add_enabled(
            !self.settings.input_only,
            egui::TextEdit::multiline(&mut self.settings.output_hex)
                .desired_rows(1)
                .desired_width(f32::INFINITY)
                .code_editor(),
        );
        if let Some(c) = &self.connection {
            if ui
                .add_enabled(
                    !self.settings.input_only && self.status.state == "Receiving",
                    egui::Button::new("Apply staged outputs"),
                )
                .clicked()
            {
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
                c.stop();
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
            ui.horizontal(|ui| {
                if ui.button("Connect").clicked() {
                    self.connect();
                }
                ui.selectable_value(&mut self.schedule, Schedule::Deadline, "Deadline");
                ui.selectable_value(&mut self.schedule, Schedule::Tokio, "Tokio baseline");
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
        if let Some(i) = i {
            let selection = self.settings.selection;
            let fields = self.settings.capture_fields.clone();
            let port = self.settings.local_udp_port;
            self.settings = self.profiles[i].settings.clone();
            self.settings.selection = selection;
            self.settings.capture_fields = fields;
            self.settings.local_udp_port = port;
        }
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
                self.identity_task.is_none(),
                egui::Button::new("Read identity"),
            )
            .clicked()
        {
            let cfg = self
                .active
                .as_ref()
                .filter(|_| self.connection.is_some())
                .cloned()
                .map_or_else(|| self.current(), Ok);
            match cfg {
                Ok(cfg) => {
                    let (tx, rx) = mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(scanner::read_identity(&cfg));
                    });
                    self.identity_task = Some(rx);
                }
                Err(e) => self.error = Some(e),
            }
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
        for (label, s) in [
            ("T->O receive", self.status.spacing),
            ("O->T send", self.tx_spacing),
        ] {
            if s.intervals == 0 {
                continue;
            }
            let percentile = |value: u64| {
                if value >= 20_000 {
                    "≥20000".to_string()
                } else {
                    value.to_string()
                }
            };
            ui.label(format!(
                "{label} spacing in µs  ·  p50 {}  ·  p95 {}  ·  p99 {}  ·  longest {}",
                percentile(s.p50_us),
                percentile(s.p95_us),
                percentile(s.p99_us),
                s.max_us
            ));
        }
        ui.separator();
        title(ui, "04", "Input assembly");
        if let Some(v) = self.status.value {
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
                ui.text_edit_singleline(&mut self.directory);
                ui.end_row();
                number(ui, "Buffer samples", &mut self.capacity);
                number(ui, "Rotate after bytes (0 = off)", &mut self.rotate_bytes);
                number(
                    ui,
                    "Rotate after seconds (0 = off)",
                    &mut self.rotate_seconds,
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
            let seen = s.seen.load(Relaxed);
            let written = s.written.load(Relaxed);
            let overrun = s.overrun.load(Relaxed);
            ui.label(format!(
                "{}  ·  Seen {seen}  ·  Written {written}",
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
                        seen.saturating_sub(written + overrun)
                    ),
                );
            }
            ui.small(s.prefix.display().to_string());
        }
    }
}
